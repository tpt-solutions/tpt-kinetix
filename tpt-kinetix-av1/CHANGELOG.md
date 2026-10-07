# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.1) - 2026-10-07

### Added

- *(stream)* RTMP AMF connect/publish + FLV depacketization, MPEG-TS HLS muxing
- DecoderCapabilities introspection + MP4 muxer crate

### Fixed

- *(av1)* read CDEF luma/chroma strength tables interleaved per index

### Other

- bump all crates to 0.1.1 and fix crates.io publishing
- add WHIP ingest, RTMP/WebSocket live hardening, and package/mux updates
- ingest policy and recording, RTMP live hardening, VP9 predict/loop-filter fixes
- all-thread sampling profiler and perf notes
- *(av1)* AVX2 warp affine core
- *(av1)* AVX2 CDEF, windowed SgrProj, deblock quick reject
- *(av1)* AVX2 8-tap MC filter rows
- *(av1)* real profile says entropy is not the bottleneck; pool the per-block Vecs
- *(av1)* decode bench, tile sub-phase timers, LTO profile, residual scratch
- AV1 SIMD helpers and bitreader speedups; H.264 env-debug hooks; VP9 div128 fixture notes
- Add ffmpeg comparison harness and fix codec bugs it surfaced
- Tidy repo root: move H.264 oracle sources and tools, drop debug artifacts
- AV1 decoder and coefficient/inter/transform updates; codec crate conformance reporting updates
- Add conformance reporting, AV1 decoder updates, and per-crate changelogs
- Rename tpt-kinetix-h264 to out-kinetix-h264 and unpublish it; AV1 decoder fixes
- trailing_bits stop bit, double filter4_clamp, inter chroma block size
- quantizer matrices (generated tables, per-segment qm level, Round2(q*qm,5) in dequant)
- film grain for 4:2:2/4:4:4 output
- lossless support (spec WHT, forced 4x4 luma/chroma tx, lossless CFL rule) and skip inter transform blocks outside the frame
- implement segmentation (segment-id coding incl. temporal prediction, alt-Q/LF/ref/skip/globalmv features, per-segment lossless, saved segment maps and feature tables)
- 10/12-bit 4:2:2 and 4:4:4 output formats; fix profile-2 10-bit depth detection and 12-bit SGR overflow
- ceil chroma width for odd-width crops; record 4:2:2/4:4:4 findings and the working dav1d-oracle recipe
- bound chroma deblock passes to the visible 4-sample units per subsampling (fixes 4:4:4 right-edge)
- code chroma transform blocks plane-major (all U then all V) per spec residual(); fixes 4:2:2/4:4:4 desync on multi-tx chroma blocks
- generalise sub-8x8 chroma MC and interintra wedge masks to 4:2:2/4:4:4; add libaom 4:2:2/4:4:4 crosscheck cases
- RefSlot chroma plane geometry honours subsampling (4:4:4 inter now bit-exact)
- stop hard-coding 4:2:0 in post-filters and LR visible extents; 4:2:2 and 4:4:4 keyframes now bit-exact vs libdav1d
- index chroma deblock metadata in 4x4 chroma cells (subsampling-aware grid for 4:2:2/4:4:4)
- fix 4:2:2 chroma row-duplication in crop_planes (transposed ss_x/ss_y)
- fix transposed 4:2:2 guard in pixel_format_for (wrong output buffer size)
- fix crop_planes chroma-stride derivation (4:2:2 decoded with a panic)
- film grain, superres, monochrome support; thread real subsampling through the tile decoder
- inter/reconstruct, loop filter, and warp/wedge updates; h264: interlaced fixes, refresh fuzz slow-input fixtures, update AV1 t2 repro test
- high-bit-depth qlookup and reconstruct updates; h264: deblock and interlaced fixes, add ITU localize debug test; update todos
- GLOBALMV and GLOBAL_GLOBALMV blocks predict with the frame's global warp model (dav1d gmv_warp_allowed); GLOBALMV with a non-translational model reads no motion_mode
- find_mv_stack applies dav1d's global-motion handling: GLOBALMV neighbours use the block's own gmv, tgmv fills empty slots and the temporal globalmv ctx, stack MVs are clamped to the frame border
- inverse_recenter in the global-motion subexp decoder follows the spec (was returning deltas); adds GMPARAMS trace
- save per-slot global motion params for PrevGmParams
- global-motion parameter constants corrected (GM_ALPHA_PREC 15, GM_ABS_TRANS 12, GM_TRANS_PREC 6, GM_TRANS_ONLY_PREC 3); subexp reference now comes from the primary reference's saved gm params
- CFL averages luma samples of a transform block that straddles the frame edge (kept in an overhang list) instead of replicating the last in-frame row/column; MaxLuma extent rounds up to the luma transform size
- skipped inter blocks mark only their block-boundary deblock edges, not interior 64x64 transform boundaries of 128-wide/tall blocks
- keep loop_filter ref/mode deltas per reference slot and restore them via primary_ref_frame (load_previous); adds LFHDR trace
- chroma deblock zero-level fallback reads the left cell for vertical edges and the above cell for horizontal edges (was swapped)
- inter blocks mark their cells in the BlockDecoded grid so later intra blocks see correct haveAboveRight/haveBelowLeft
- inter blocks write DC_PRED into the chroma-mode neighbour context so get_filter_type does not see stale SMOOTH chroma modes
- KINETIX_AV1_DUMP_FRAMES names files by decode-order frame number
- skip deblocking of a chroma plane whose frame loop_filter_level is zero (deltas must not raise it); adds KLF deblock trace hook
- motion-field save uses the bottom-right 4x4 of each 8x8 cell (dav1d save_tmvs reads rt->r+6, block row 2y+1); adds RP/SV dump debug hooks
- find_mv_stack adds temporal candidates before the corner/secondary spatial scans (dav1d and spec order); fixes ordering of equal-weight entries
- parse temporal_point_info, current_frame_id and delta_frame_id_minus_1 in the frame header; accept show_existing_frame with decoder model / frame ids
- av1_fate_score slices the reference by each decoded frame's own size (ffmpeg -noautoscale) so resolution-switching streams score correctly
- fix compound extended-candidate array indexing and skip_mode ctx clearing; scaled-reference MC (switch_frame 30/31 now match dav1d)
- document session 2026-09-29 #4 (switch_frame 30/31 root cause found, not yet fixed)
- implement frame_size_with_refs() found-ref search, fix switch_frame frame 31 tile bug
- fix frame_size() width/height parsing for error-resilient SWITCH_FRAME resolution changes
- clamp CFL MaxLumaW/MaxLumaH to the tile buffer edge, not the block's nominal extent
- fix has_overlappable_candidates OBMC-eligibility scan (switch_frame frame 3 desync)
- fix switch_frame frame 2 chroma to bit-exact (105 -> 0 diff bytes)
- fix tl_4x4_filter tracking for sub-8x8 chroma diagonal quadrant
- attribute the switch_frame frame-2 chroma error to inter chroma MC
- record that the fh-only 8-tap rounding constant 34 is correct
- localize switch_frame frame 2 — chroma-only, and upstream of deblock/CDEF
- REVERT h264 deblock "Table 8-16 floor" — it was a measured regression, and
- read_lr gating now matches dav1d (unit-alignment + frame-boundary) — the 106 extra entropy reads per tile row are gone; the EC desync at the non_uniform_tiling (112,56) skip read should be resolved
- desync mechanism exact — K performs extra entropy reads dav1d doesn't (2 at tile start + 1 zero-cost mid-tile read at tile row 2 of oh=3); the extra reads adapt CDF cells dav1d never touches, flipping later reads. KSEQ prints now tagged; next: caller-location capture and read_lr gating fix
- divergence localized to (120,48)'s MV-mode cascade — four mode bools, same ctx 0x33, states diverge between ref (55492 matched) and interintra (37000 vs 46680); pre-read states at (112,56) captured on both sides
- tile-2 desync root found — K's MV stack for (116,48) leads with mv=(4,-8), neither left candidate; temporal-candidate rank or above-tile leak
- correct the record — tile-2 divergence is recon-level EC desync, not CDEF; tile group sizes verified equal in both decoders
- deepest root isolated — single-bit EC decoder divergence at frame oh=3 tile-2 block (112,56)
- proven prediction/coeffs/ITX/add byte-exact on the frame-3 root block; remaining diffs isolated to CDEF secondary filtering
- retargetable warp per-tap debug hooks; document the (116,44) warp-block investigation
- refreshed DPB slots always receive the frame's CDF context; official FATE 3/195 -> 9/195
- fix t2.ivf chroma-only inter regression (co_located_luma_type eob>0 gate)
- bin-by-bin coefficient-entropy audit for the t2.ivf chroma bug (session cont'd 27)
- document two disproven co_located_luma_type hypotheses (session cont'd 26)
- widen KINETIX_AV1_MCSUM to chroma, fix its ix/iy shift bug; rule out MC/txtype/inverse-transform for the t2.ivf chroma bug
- clamp inter reference reads at the visible frame dims (dav1d emu_edge), not the mi-grid extent
- add the frame-0 CDEF direction-search probe unit test (test vector from the dav1d comparison)
- add KINETIX_AV1_DUMP_GRID hook to dump the stored reference grid plane
- CDEF processes the full mi-grid extent; frame 0 of non_uniform_tiling is pixel-exact vs dav1d
- revert the CDF rate formula change; pin the empirical rate with a warning
- anchor filter/tx-split reads for frame 4; desync narrowed inside the coefficient read
- fix missing BLOCK_4X4 gate on read_tx_size for intra-in-inter blocks
- extend inter-block debug probes for the entropy-desync hunt
- fix an out-of-bounds panic in the MCSUM MC probe; make it targetable
- make the KINETIX_AV1_DBG_PRED probe targetable instead of hardcoded
- make the debug frame counter unconditional; scope pixel traces to a frame
- split the intra/var-tx transform contexts and fix the inter tx_intra write
- tag motion_mode/partition-CDF debug prints with block positions; document frame-4 filter-read desync
- widen PARTCDF hook to 64x64 ctx2 and tag with block position; document frame-4 entropy divergence
- scale the inter-intra blend ramp index by block size
- fix inter-intra blend masks and tile-relative edge availability
- fix inter prediction reading the reference at tile-local coordinates
- implement non-uniform tile_info, per-tile tile-group splitting, and tile-relative partition bounds
- persist field-picture motion grid and ref-list POCs for B-field direct mode
- fix MBAFF B-slice field-picture ref indexing and chroma MC
- add KINETIX_AV1_DBG_PREDUMP2 probe for the OH=7 chroma V MC prediction
- extend KINETIX_DBG_TAP67 to also dump the U plane
- add KINETIX_DBG_TAP67 pre-CDEF dump probe, extend uv-cf-blk trace and obu-dump harness
- TAPBLK probe also prints the full ref_to_slot/dpb_order_hints tables
- extend TAPBLK probe with resolved ref slot/order_hint; add 160x90 grid-dump harness
- add KINETIX_AV1_DBG_TAPBLK probe for the 160x90 CDEF tap-source block
- fix tile_cw/tile_ch to match the spec's 8-pixel-rounded MI grid
- fix skip-mode/no-chroma inter blocks never marking chroma deblock edges
- HCAFR1 8x8 triage, AV1 inter fixes, VP9 decoder overhaul
- OBMC deep trace for chroma (mi 4,20) with before-blend values; docs — MM read count mismatch and show_existing replay decode lead
- env-gated chroma MC trace (KINETIX_DBG_MCCHK) in inter_predict_plane
- sample temporal motion field from the cell's odd 4x4 column
- refmvs context derivation — match dav1d refmvs.c exactly
- replace VP9 subpel filters with the AV1 6-bit table (dav1d port)
- scaffold new tpt-kinetix-vp9 decoder crate
- fall back to the block's own luma tx type for sub-8x8 chroma
- honor disable_cdf_update in symbol reads (§6.8.2)
- derive GLOBALMV motion vectors from the frame's global motion params
- fix SgrProj 5x5 pass even-row A/B sampling (six-neighbors y-1/y+1)
- record skip-mode blocks' ref/mode for the deblock filter level
- gate CDEF filtering on the §7.15.1 per-8x8 noskip mask
- fix backwards dual-filter horizontal/vertical assignment in MC
- fix CDEF primary tap formula; remove debug traces
- run deblock/CDEF/LR post-filters on the assembled frame, fix CDEF tap direction
- keep per-direction MC filter plumbing; document 6-bit subpel experiment
- apply per-direction interpolation filters in motion compensation
- implement inter-intra prediction, fix OBMC masks and intra neighbour leak
- carry inter-symbol CDFs across frames and reset counters on save (6.8.2)
- port dav1d's temporal MV projection model and mv-stack extended search
- align secondary mv-stack scans to odd mi positions (7.10.2.2/7.10.2.3)
- gate inter chroma coefficient reads on §7.3.1 has_chroma
- entry-rng block trace hook (KINETIX_AV1_DBG_B0ENTER) for symbol-level diff vs patched dav1d
- derive inter chroma tx type from the co-located luma leaf
- per-block deblock levels from recorded block ref/modeType (§7.14.4/§7.14.5)
- per-edge deblock levels from recorded block ref/modeType (§7.14.4/§7.14.5)
- implement §6.8.2 CDF context save/restore — fixes the inter-frame entropy desync
- fix OBMC blend mask weighting per §7.11.3.9 + env-gated debug traces
- implement local warped motion (block_warp_process, WARP motion_mode)
- mark inter-intra neighbours as ref[1]=INTRA_FRAME in the warp-samples grid
- fix find_warp_samples spurious scan-wide stop (§7.10.4.2)
- implement temporal MV candidates (motion_field_projections, §7.10.2)
- KINETIX_AV1_DBG_WARP also traces the resolved cur_mv/threshold
- KINETIX_AV1_DBG_WARP traces find_num_warp_samples's add_sample scan
- real find_warp_samples NumSamples for read_motion_mode's CDF gate
- use the real non-keyframe y_mode CDF for intra blocks in inter frames
- reset chroma coeff neighbour context on a skipped inter transform
- DBG_B0 traces per-coeff-block eob/txtp; locate real inter chroma desync
- DBG_B0 vartx trace prints full leaf list, not just the first
- hidden-frame temporal units emit Ok(None), not a grey placeholder
- regression tests for the large (64-family) inverse transforms
- KINETIX_AV1_NOLR env hook to skip loop restoration
- apply OBMC (overlapped motion compensation)
- wedge + difference-weighted masked compound blend
- DC-only inverse-transform coverage for TX_{16X32,32X16,32X64,64X32}
- note the large-tx inter residual is rng-verified but not yet applied
- compound blend in the intermediate domain (prep + avg/w_avg)
- chroma MC at 1/16-pel precision
- fix stale motion_compensate doc comment (rounding)
- fix MC subpel rounding to §7.11.3.3 + horizontal-pass range bug
- note compound entropy chain fully verified; large-tx inter residual gate stays
- compound Stage 3 — read_compound_type (§5.11.26)
- compound Stage 2 — comp_inter_mode (8-way) + drl + per-ref MV
- compound Stage 1 — is_comp flag + ref-frame tree + §8.3.2 contexts
- decode inter residual via the real var-tx tree (§5.11.16)
- implement read_interintra_mode (§5.11.28) for inter blocks
- implement read_motion_mode (§5.11.23) for inter blocks
- av1 inter — blocks 0/1 mode reads bit-exact; corpus +8dB; next desync noted
- correct intra_inter (is_inter) neighbour context (\xc2\xa78.3.2)
- ZeroMvContext = use_ref_frame_mvs init (\xc2\xa77.10.2 temporal sample)
- real single_ref_frame tree + per-symbol contexts (\xc2\xa78.3.2)
- real find_mv_stack for inter blocks + spec single-ref mode cascade
- generalise RefMvCell to carry ref pair + MV pair + motion flags
- skip deblock entirely when both luma loop-filter levels are 0
- decode skip_mode blocks (compound, NEAREST MV, no residual)
- real skip_mode_params + multi-frame temporal units + TU-aware inter harness
- KINETIX_AV1_DBG_SEQ hook; document inter skip_mode gap
- implement show_existing_frame (display a DPB slot, no reconstruction)
- KINETIX_AV1_DBG_FH hook; document inter conformance-harness mismatch
- dual switchable interp-filter read + 16-way context (inter infra)
- arm Phase G intra bit-exact gate; correct stale capabilities notes
- bilinear sub-pel for IBC chroma prediction (testsrc2 now fully pixel-exact)
- implement FLIPADST inverse transforms (testsrc2 luma now pixel-exact)
- port find_mv_stack for intrabc DV prediction (testsrc2 36->61 dB Y)
- fix IBC displacement-vector predictor + copy sign (testsrc2 24.7->36.2 dB Y)
- loop restoration reads stripe-boundary rows from the pre-CDEF plane
- index chroma-mode neighbour context on the chroma grid
- don't clobber chroma-mode neighbour context from no-chroma blocks
- fix luma CDEF variance strength cap (12, not 8)
- fix two chroma CDEF bugs — testsrc now fully pixel-exact
- fix CI rustfmt violations, gate stray tile_init debug print
- fix narrow deblock filter to apply one clip per spec §7.14.6.3
- remove CFL/pixel-trace debug instrumentation; fix clippy manual_clamp in cdef variance
- add CFL and per-pixel reconstruction debug tracing
- revert pri_str_orig CDEF tap-selection regression
- fix loop filter Round2, CDEF tap selection, and chroma direction
- resolve ref_idx/ref_idx_l1 to POC before deblocking in single-slice B path
- swap RefPicList1[0]/[1] when it is identical to RefPicList0 (8.2.4.2.3 Note 2)
- fix CABAC multi-slice end_of_slice handling; fix SPS/PPS-by-id selection; MIDR_MW_D/MPS_MW_A investigation progress
- wire per-block delta_lf into compute_level; fix inter-block FrameMeta geometry gap
- fix hidden-divergence debug threshold; localize mandelbrot's real first pixel gap
- fix two Python-oracle bugs that caused false mandelbrot/testsrc entropy divergence
- fix broken merge 46cfd2e (duplicated deblock/coeff-context defs)
- Merge branch 'master' of https://github.com/tpt-solutions/tpt-kinetix
- add PATENTS.md and gate H.264/AAC behind default-on Cargo features
- fix chroma deblock coordinate mapping (w8 vs luma-4px grid)
- track chroma tx boundaries; fix over-deblocking of chroma planes
- deblock at 4-sample granularity; fix CDEF chroma direction
- add palette reconstruction debug traces; enhance psnr_check row diff
- fix IBC source position sign + disable broken loop restoration apply
- implement IBC intra-block-copy reconstruction; add SB1/IBC/BITS/ymode debug hooks
- implement loop restoration (Wiener + SgrProj, §7.17)
- fix three CDEF bugs — correct direction detection, packing, and sec strength
- seven reconstruction fixes + spec-correct MV component parsing
- fix tx_depth context sentinel for unavailable neighbours
- CDEF per-64x64-unit strength via cdef_idx; AAC conformance/debug cleanup
- AAC PNS noise-scalefactor fix, HLS segment roll-over, and volumetric TMC13 geometry cross-check
- mbaff_ibp B frame bit-exact — B_8x8 mvd order + bi-pred 8x8 transform recon
- remove reference C/oracle files; vision: overhaul reconstruct + deblock/prediction/headers; h264: interlaced + cabac + ref_pic updates; av1: reconstruct/partition; screen/lean/lossless/cli updates
- fix interlaced field pairing and CABAC 8x8 residual permutation
- implement CABAC transform_size_8x8_flag + 8x8 inter residual for P/B; remove MBAFF_FIELD_MC gate (B5); verify A3 ref_idx geometry + add ctx.rs unit tests; av1: replace partition context 1D arrays with true 2D MiSizes
- implement full block reconstruction (intra+inter, WHT, deblock) with DPB; screen: implement mode classifier + flat/glyph/NATURAL reconstruction; vision: add headers/prediction/quant/transform/deblock/reconstruct modules; av1: remove partition debug instrumentation; update todos
- reconstruct/deblock at coded dimensions, crop to visible on output; av1: add inverse-transform/filter-intra/palette tests; cli: implement transcode and stream commands
- gate cabac_b.rs debug lines behind KINETIX_BINTRACE; aac: add Phase 5 window-sequence proptest + rustfmt/clippy cleanup; av1: rustfmt
- add MBAFF pair-scan MV prediction; av1: extend entropy/oracle traces + tile-level oracle; aac: add CPE channel-pair debug instrumentation
- fix MBAFF B-slice CABAC grid addressing; aac: add TDAC/TNS/M/S debug instrumentation; av1: add targeted tx-block debug + rustfmt
- skip end_of_slice_flag after TOP MB in MBAFF CABAC I/P/B slices
- extend deblock/interlaced reconstruction + CABAC-I/CAVLC; av1 entropy/reconstruct fixes; aac syntax; debug tooling
- extend CABAC entropy decode + reconstruct; add debug tooling
- extend CABAC P/B-frame entropy decode; aac/av1 fixes and debug tooling
- extend CABAC B-frame entropy, AV1 reconstruct CDFs, AAC PNS; add debug tooling
- align deblock bS with ffmpeg, extend AV1 reconstruct + AAC stereo; add debug tooling
- fix AAC PNS noise correlation; rustfmt + debug tooling
- refine CABAC B-frame, AV1 coeff decode, AAC PNS/scalefactors; extend oracle + debug tooling
- advance CABAC B-frame, AV1 coeff/entropy, AAC PNS; add AV1 oracle tooling
- advance CABAC B-frame decode, motion vectors, AAC window/MDCT/PNS; add fuzz + conformance
- advance CABAC P/B decoding, motion vector, and deblock; refine AAC/AV1
- fix section_cb decoding and ics_info parsing; harden h264 CABAC and transform
- fix intra prediction and CAVLC, guard AAC conformance test
- apply rustfmt and minor cleanup across codecs and tests
- fix intra_mode_context table and SMOOTH_PRED/D207_PRED contexts; harden H.264 CABAC MVD neighbor handling
- add coefficient decode and reconstruction support; trim high-profile conformance test
- Modularize AV1 reconstruct and H.264 decoder; add H.264 pixel/conformance tests
- Wire intra-edge-filter/CFL/palette and fix H.264 CABAC P/B decode
- Fix AAC/AV1/H.264 decode paths and expand transform-size coverage
- Expand AV1 coefficient scan tables to all TxSizes and wire filter-intra prediction
- Fix decoder bitstream-desync and dequant bugs in AV1, H.264, and AAC
- Advance AAC, AV1, and H.264 decode paths, plus realtime and face codec scaffolding
- Advance AV1 reconstruction, H.264 CAVLC slice paths, and AAC decode modules
- Advance AAC syntax/decoder, AV1 reconstruction, and H.264 CAVLC slice paths
- Advance AAC decode modules, AV1 reconstruction, and H.264 high-profile paths
- Advance AAC decode modules, AV1 inter prediction, and face codec scaffolding
- Expand AAC decoder modules and advance H.264/AV1/realtime decode paths
- Advance H.264/AV1 decode paths and AAC codebook integration
- Advance H.264/AV1 decode paths and add volumetric codec scaffolding
- Merge branch 'master' of https://github.com/tpt-solutions/tpt-kinetix
- Advance H.264/AV1 decode paths with reconstructed reference picture handling and diagnostics
- Advance H.264/AV1 decode paths and lossless codec wiring
- Add new codec crates (face, lossless, realtime, screen, volumetric) and bitstream foundation
- Implement H.264 scaling lists and 8x8 inverse transform
- Rework AV1 inverse transforms and harden H.264 CABAC MVD decoding
- Implement AV1 Phase C superblock decode and fix H.264 P/B CABAC mb_type and CBP logic
- Add H.264 CABAC inter-slice decode paths and AV1 OBU/keyframe conformance harness
- Add B-slice conformance tests, AAC/bench-report tooling, and CABAC scratch harness
- Implement H.264 MMCO/dec_ref_pic_marking and reference-picture list wiring
- Add AV1 entropy decoding with CDF tables and extend H.264 P-slice coverage
- Fix H.264 Intra_4x4 DiagonalDownRight prediction and deblock OOB panic
- Wire H.264 CAVLC I-slice decode into decoder.rs, scaffold tpt-kinetix-vision crate
- Enable H.264 intra prediction and deblocking, add CABAC I-slice contexts
- Fix cargo fmt violations flagged by CI
- Fix remaining unnecessary_cast clippy lint in av1 frame.rs
- Clean up av1 clippy lints and fix rtmp AMF strict-array OOM
- Fix pre-existing workspace build failures unblocking CI
- Add README/wasm browser demo, AV1 frame scaffold, and Phase 11 adoption polish
- rename kinetix-* crates to tpt-kinetix-*, add probe subcommand and CI jobs

### Changed

- **Performance: the deblocking loop filter no longer allocates.** The filter
  is the hottest phase of AV1 decode (Phase 2 of `todo-perf.md` measured it at
  62% of a 320x240 frame and 70% at 720p), and it was heap-allocating **two
  `Vec<i32>` per filtered row** — a line buffer and the result — inside the
  innermost loop. Both deblocking passes now filter through a reusable
  16-sample stack buffer, since the filter only ever reads 7 taps before and 6
  after the edge. Measured on the cached `testsrc` corpus (decode only, phase
  timers off, A/B by stashing `loop_filter.rs`):
  - 320x240: 36.23s -> 30.42s for 12000 frames (**-16.0%**)
  - 1280x720: 130.59s -> 110.71s for 3600 frames (**-15.2%**)
- **Internal refactor:** `filter_line_1d` is split into an allocating
  `filter_line_1d` wrapper and an allocation-free `filter_line_1d_into` core
  that writes into a caller-owned buffer. The allocating form is now
  `#[cfg(test)]` and is kept as the reference oracle for the shared-buffer path,
  which is what the existing filter unit tests exercise.

Decoded output is **unchanged**: the FATE corpus is still 204/204 bit-exact vs
libdav1d, `libaom_crosscheck` and `phase_c_conformance` still report zero luma
difference, and all 173 AV1 lib tests pass.

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- *(stream)* RTMP AMF connect/publish + FLV depacketization, MPEG-TS HLS muxing
- DecoderCapabilities introspection + MP4 muxer crate

### Other

- rename kinetix-* crates to tpt-kinetix-*, add probe subcommand and CI jobs
