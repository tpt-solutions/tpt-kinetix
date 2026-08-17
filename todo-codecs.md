# TPT Kinetix — Specialist Codecs Todo

> Original codec work (lean, vision, realtime, lossless, screen, face, volumetric).
> See [todo.md](todo.md) for the project index.

## Phase 13 — `tpt-kinetix-lean`: Original Embedded-First Codec (2026-07-20)

> Goal: an original (not ported) codec design prioritizing bounded-memory,
> bounded-time decode on constrained hardware over maximum compression ratio.
> Independent track — does not block on or get blocked by Phase 12. Accepts
> ~10-15% worse compression than AV1 in exchange for a decoder implementable
> in a few thousand lines with genuinely parallel entropy decode.

### Design
- [x] Write the format design doc: header layout, fixed block-partition
      scheme, rANS stream-interleaving/framing — documented in
      `tpt-kinetix-lean/src/headers.rs` (byte layout table) and
      `tpt-kinetix-lean/src/rans.rs` (stream-set framing) module docs;
      integer transform sizes, intra mode set (14 modes), inter/motion-vector
      approach (unidirectional, quarter-pel, 6-tap), in-loop filter placement
      (single-stage deblocking) documented in `tpt-kinetix-lean/src/lib.rs`
      crate-level docs
- [x] Document the memory/perf budget for v1 (target max resolution, arena
      size ceiling, per-frame decode time budget) sized for embedded-Linux
      SBC-class hardware (e.g. Raspberry Pi–class), noted as revisitable —
      `tpt-kinetix-lean/src/lib.rs` crate-level docs, "v1 target envelope"

### Scaffold
- [x] Generate `tpt-kinetix-lean` crate from `templates/codec-crate/`, add to
      workspace `members`
- [x] Implement `headers.rs`: sequence/frame header struct definitions
      (max dimensions, max ref count, block size range, quant params)
- [x] Implement `bitreader.rs`: bit-level reader for the new format
- [x] Implement `rans.rs`: rANS/tANS encode/decode primitives + stream
      interleaving skeleton, with a stubbed extension point for the
      per-symbol probability model — `RansEncoder`/`RansDecoder` are real
      and round-trip-tested against a uniform `StaticModel`; the adaptive/
      context-selected `SymbolModel` real coefficient coding will use is
      the still-open extension point
- [x] Implement `decoder.rs` shell: `DecoderCapabilities` honesty pattern
      (`pixel_exact: false`, `with_strict()` → `NotPixelExact`), header
      parsing entry point
- [x] Add `fuzz/` target for the header/bitstream parser
- [x] Add round-trip test scaffolding (header parse round-trip;
      `DecoderCapabilities` not-pixel-exact contract) — full encode/decode
      round-trip test lands once reconstruction exists

### Open questions (resolved)
- [x] Decide whether to factor a shared `tpt-kinetix-bitstream` utility crate
      now that this would be the second hand-rolled bit reader in the
      workspace (alongside `tpt-kinetix-h264/src/bitreader.rs`), or keep them
      independent per-codec — **Decision: start independent, extract later.**
      Both `tpt-kinetix-lean` and `tpt-kinetix-vision` carry their own
      `bitreader.rs` / `rans.rs` copies. A shared `tpt-kinetix-bitstream`
      crate will be extracted once both codecs are stable enough to freeze the
      rANS interface (documented in `docs/vision-codec-design.md` DECISION 8
      and `tpt-kinetix-lean/src/lib.rs`).
- [x] Decide the no_std/MCU port plan and timeline once the v1 alloc-free
      hot path is in place and proven on embedded Linux — **Decision: v1
      targets embedded Linux (prove the alloc-free hot path first); no_std/MCU
      is a v2 target.** Both `tpt-kinetix-lean` and `tpt-kinetix-vision` design
      docs note this explicitly.

## Phase 14 — Specialist Codec Roadmap (backlog) (2026-07-20)

> Status: backlog only — none of these are started. Full rationale table
> lives in `docs/codec-backlog.md` ("Original Specialist Codec Concepts");
> this phase tracks getting each one designed/scaffolded when prioritized,
> the same way Phase 13 was written only once `tpt-kinetix-lean` was
> actually decided on. Do not start implementation on any of these without
> first writing its own Phase-13-style design section here.

- [~] `tpt-kinetix-vision` — video-for-machines: optimize for detector/
      classifier accuracy per bit rather than human perceptual quality;
      chroma-optional, model-matched bit depth, tensor-output decode path
      (see `docs/codec-backlog.md` for design notes) — design phase started,
      see Phase 15

> Expanded 2026-08-06: the five items below were each a single line standing
> in for an entire future effort the size of Phase 13/15 (design doc →
> scaffold → implement). None of this is active work — the "Prioritize this
> list" gate at the bottom still applies, and per the rule above, real
> implementation on any of these still waits on its own design section (the
> same one Phase 15 wrote for `tpt-kinetix-vision`). What's new is that each
> codec now has its *design-phase* checklist pre-drafted, grounded in the
> rationale already in `docs/codec-backlog.md`, so whichever one gets picked
> next has a session-sized starting point instead of a blank page.

#### `tpt-kinetix-realtime` — design-phase checklist (RESOLVED 2026-08-13; promoted to Phase 16 scaffold)
- [x] Decide the target profile for v1 — **profile-agnostic** (2026-08-13):
      design the bitstream around the shared realtime core (no-B-frame
      lookahead / low-latency GOP + partial-frame loss recovery) and expose
      cloud gaming, video conferencing, and AR/smart-glasses overlay as
      *decode profiles / config knobs* (latency ceiling, power budget,
      foveation enable, loss-resilience strength), not as separate codecs.
      Rationale: (a) all three share the same latency + loss-resilience center,
      so a single forkable core avoids 3× surface; (b) `docs/codec-backlog.md`
      flags AR overlay as the most demanding of the three (extreme power
      budget, foveated/gaze-contingent rendering, real-world latency), so AR
      becomes the *hardest* profile (the stress test), not the v1 baseline —
      it leans on `tpt-kinetix-lean`'s power-conscious embedded primitives
      rather than being invented independently here; (c) cloud-gaming and
      conferencing are near-identical profiles differing only in decode
      complexity + symmetry, so they collapse to one default profile with two
      preset parameter sets. Next items below should design the core to be
      profile-parameterized from day one.
- [x] Write the format design doc: partial-frame loss-recovery mechanism
      (forward error correction vs. concealment vs. both) and how the
      no-B-frame-lookahead constraint shapes GOP structure — **designed
      (2026-08-13), see `docs/realtime-codec-design.md`** (DECISION 1 hybrid
      FEC + intra-refresh + concealment; DECISION 2 rolling intra-refresh +
      on-demand IDR, single reference, no B-frames)
- [x] Design the per-frame latency budget and how it's enforced (encode-side
      deadline, decode-side bounded work) — **designed**, see
      `docs/realtime-codec-design.md` DECISION 4 (encode `deadline_ms` rate
      control fallback + decode bounded work via no-B / single-ref / fixed
      slice grid / lean-style single deblock + `max_decode_ms` capability)
- [x] Decide how loss resilience is measured for design validation (e.g. a
      simulated packet-loss-vs-quality curve, the loss-resilience analogue of
      Phase 15's mAP-vs-bitrate metric) — **decided**, see DECISION 5:
      PSNR/SSIM-vs-loss curve + stall/freeze-rate-vs-loss (latency half);
      gaze-weighted variant deferred to AR profile; harness in
      `tpt-kinetix-test-utils` behind `realtime-bench`
- [x] Document the memory/perf/latency budget for v1 (target hardware class,
      ms/frame ceiling) — **documented**, see DECISION 6: one envelope with
      three profile presets (cloud gaming / conferencing / AR-foveated); AR
      is the hardest preset (fine slice grid, 20-30% FEC, <10 MB arena,
      foveation on)
- [x] Decide the relationship to `tpt-kinetix-lean` — `docs/codec-backlog.md`
      notes they share a power/latency-conscious embedded target — **decided**,
      see DECISION 7: extract `tpt-kinetix-bitstream` now (lean + vision +
      realtime are 3 copies of `BitReader`/rANS/partition/transform/deblock);
      realtime adds slice-grid framing, intra-refresh masking, FEC framing,
      optional foveation, latency-deadline fields on top

#### `tpt-kinetix-lossless` — design-phase checklist (start here once prioritized)

> v1 scope **decided (2026-08-13):** a single unified lossless format serving
> all three target domains (medical imaging, scientific capture, archival),
> guaranteeing bit-exact round-trip for **10/12/16-bit** samples. Design doc:
> `docs/lossless-codec-design.md` (DECISION 1 resolved; 2–6 specified).
>
> **Crate scaffolded (2026-08-13):** `tpt-kinetix-lossless` exists with a
> working bit-exact reversible predictive path (median predictor + adaptive Rice
> entropy + per-plane CRC, reusing `tpt-kinetix-bitstream` primitives). 12 unit
> tests pass (10/12/16-bit + multi-plane round-trips, corrupted-checksum
> rejection, reserved-`transform_id` rejection). Next: rANS swap-in + wavelet
> mode + ratio harness (DECISION 4).

- [x] Decide the target domain for v1 (medical imaging vs. scientific capture
      vs. archival) and the bit-depth range it must support (10/12/16-bit)
      — **unified across all three domains; 10/12/16-bit bit-exact** (DECISION 1)
- [x] Write the format design doc: reversible compression approach
      (predictive + entropy coding, à la FFV1, vs. a reversible wavelet
      transform) — **done**, `docs/lossless-codec-design.md` DECISION 2:
      predictive+entropy (FFV1-like) primary; reversible wavelet mode reserved
      via `transform_id` for a later phase
- [x] Design the decode path's correctness contract — how bit-exact
      round-trip is verified as part of the format itself (e.g. a built-in
      checksum), not just left to external testing — **designed** (DECISION 3):
      per-frame CRC32/CRC64 + stream SHA-256, decoder returns `ReversibilityError`
      on mismatch; round-trip harness in `tpt-kinetix-test-utils`
- [x] Decide how compression ratio at guaranteed losslessness is measured for
      design validation (baseline against FFV1 / lossless HEVC) — **decided**
      (DECISION 4): ratio-vs-FFV1 bpp metric on a 10/12/16-bit corpus, within
      ~10% of FFV1 as the v1 acceptance bar
- [x] Document the memory/perf budget for v1 (archival-quality capture means
      large uncompressed frame sizes) — **documented** (DECISION 5): ≤4096²
      per plane, bounded arena via `max_*` header, parallel rANS sub-streams,
      integer-only math, no real-time budget
- [x] Decide the relationship to existing kinetix bitstream primitives
      (reuse `tpt-kinetix-lean`'s `bitreader.rs`/`rans.rs` shape or diverge) —
      **decided** (DECISION 6): depend on `tpt-kinetix-lean` for `Rans`/
      `BitReader`; add a new reversible prediction stage (Lean's DCT bank is not
      reused)

#### `tpt-kinetix-screen` — design-phase checklist (start here once prioritized)
- [x] Decide the target content class for v1 (desktop capture vs. mobile UI
      vs. mixed screen+video content) — **Decision: desktop capture** (mobile
      UI → higher-DPI profile, mixed screen+video → stretch/validation corpus);
      see `docs/screen-codec-design.md` DECISION 1
- [x] Write the format design doc: per-block mode classification (flat-fill
      run-length, glyph/edge palette mode, natural-image fallback) — **DECISION 2
      resolved** in `docs/screen-codec-design.md` (fixed 16x16 CB grid + mode map
      rANS stream + FLAT run-length + GLYPH dict ref + NATURAL reuses Lean)
- [x] Design cross-frame glyph/palette dictionary reuse for repeated UI
      elements — **DECISION 3 resolved** in `docs/screen-codec-design.md`
      (encoder-authoritative global glyph dict + color palette, inline-at-first-use,
      key-frame/flag reset; no implicit LRU)
- [x] Decide how compression efficiency is measured for design validation
      (baseline against H.264 screen-content-coding extensions or VP9
      lossless) — **DECISION 4 resolved** in `docs/screen-codec-design.md`
      (PSNR RD vs H.264 SCC + VP9 lossless primary; edge-F1 differentiator;
      VMAF guard; ffmpeg-gated `screen-bench` harness)
- [x] Document the memory/perf budget for v1 — **DECISION 5 resolved** in
      `docs/screen-codec-design.md` (1080p/60fps, dict_cap 256, palette_cap 64,
      glyph_max_dim 32, ~12 MB arena, <8 ms RPi-5 decode)
- [x] Decide the relationship to existing kinetix bitstream primitives —
      **DECISION 6 resolved** in `docs/screen-codec-design.md` (depend on
      `tpt-kinetix-bitstream` for BitReader+rANS+SymbolModel; NATURAL fallback
      reuses transform/partition/intra/deblock from bitstream once promoted, lean
      interim; screen owns only classifier/dict/palette/RLE)

#### `tpt-kinetix-face` — design-phase checklist (start here once prioritized)
- [x] Decide the landmark/parametric face representation for v1 (3DMM, sparse
      keypoints, or a learned latent code) — **RESOLVED: parametric 3DMM-style
      head model is the primary v1 representation; sparse landmarks are a
      low-bitrate companion; learned latent is deferred to v2.** Encoded in
      `tpt-kinetix-face/src/representation.rs` (`FaceRepresentation` enum +
      `V1_3DMM_DIMS`); matches `docs/face-codec-design.md` DECISION 1. `FaceParams`
      already carries the 3DMM coefficient groups.
- [x] Write the format design doc: keyframe (real pixel image) + per-frame
      parameter-delta bitstream — a materially different shape from every
      pixel-coding entry in this table — **DONE: `docs/face-codec-design.md`
      resolves DECISION 3** (byte-aligned sequence header + one-time identity
      setup + per-frame rANS-coded expression/pose deltas; `magic`/`version`/
      `basis_hash` pin; companion landmark block).
- [x] Design the decode path's dependency contract: synthesis requires
      running a generative model at decode time, so decide whether that model
      ships with the decoder or is a pluggable external dependency — **DONE:
      DECISION 7 (standalone sibling crate, depends only on `core`, reuses
      `tpt-kinetix-bitstream` rANS) + DECISION 8 (v1 synthesizer is a
      deterministic 3DMM rasterizer shipped with the decoder — zero NN weights
      on the decode path, so no pluggable external dependency for v1).**
- [x] Design the fallback/failure path for content the trained face model
      can't represent (occlusion, non-face content, extreme pose) — **DONE:
      DECISION 8** (basis asset missing / hash mismatch →
      `KinetixError::NotPixelExact`; missing optional neural-texture model →
      graceful rasterizer fallback reported via capability; corrupt payload →
      rANS decode error; off-manifold content is an accepted conferencing
      limitation, not a crash).
- [x] Decide how synthesis quality is measured for design validation
      (perceptual similarity to source, not a pixel-exact diff) — **DONE:
      DECISION 5** (primary = LPIPS reconstruction + ArcFace identity
      similarity, gated behind `face-bench`; cheap deterministic CI gate =
      landmark NME regression).
- [x] Document the memory/perf budget for v1 — unlike every other entry here,
      the generative model's inference cost is part of the decode budget —
      **DONE: DECISION 6** (conferencing-endpoint envelope like `lean`/`vision`;
      0 decoder NN weights; ~20 MB arena @1080p; <10 ms/frame; v2 neural-texture
      layer capped ~1–5 MB).

#### `tpt-kinetix-volumetric` — design-phase checklist (start here once prioritized)

> **Resolved 2026-08-15.** All six items map to `DECISION:` blocks in
> `docs/volumetric-codec-design.md`. Items 1/2/3/5 were already covered by
> DECISION 1/2/3/7/8; items 4 (efficiency measurement vs Draco + TMC13) and 6
> (explicit shared-primitive statement) were added as DECISION 9 and DECISION
> 10 this revision to close the two gaps that had no prior `DECISION`. See the
> "todo.md design-phase checklist — item → decision map" table in that doc.
> (The full scaffold + pre-scaffold work is tracked under Phase 15 below,
> `tpt-kinetix-volumetric`.)

- [x] Decide the target volumetric representation for v1 (point cloud vs.
      voxel grid vs. mesh+texture) — **DECISION 1: point cloud**
- [x] Write the format design doc: since this is 3D, not 2D-frame, data,
      decide whether it reuses any `tpt-kinetix-core` frame/packet types or
      needs new core types entirely — **new `PointCloud` core type; reuses
      only `KinetixError`/`DecoderCapabilities`/`Packet`/`Timestamp` contract
      plumbing, never `VideoFrame`/`AudioFrame`/`PixelFormat` (item 2 note)**
- [x] Design the spatial-partitioning + entropy-coding approach for
      point/voxel data (e.g. octree) — **DECISION 2 octree + DECISION 3
      lift/RAHT attributes + DECISION 7 rANS from `tpt-kinetix-bitstream`**
- [x] Decide how compression efficiency is measured for design validation
      (baseline against Draco / MPEG point-cloud-compression) — **DECISION 9:
      D1/D2 geometry PSNR + per-attribute PSNR at bits-per-point, baselined
      against TMC13 (oracle) and Draco (external third-party)**
- [x] Document the memory/perf budget for v1 — **DECISION 8: 10M-point cap,
      ~64 MB arena, server/edge class**
- [x] Confirm explicitly that this shares no primitives with any other
      kinetix codec — `docs/codec-backlog.md` flags it as fundamentally
      different, but that should be a stated decision here, not an assumption
      — **DECISION 10: shares zero codec-domain primitives; deliberately
      reuses `tpt-kinetix-bitstream` (rANS) + `tpt-kinetix-core`
      (output/error/capability) infra, same as every other original codec**

- [x] Prioritize this list (pick the next one to move from backlog to an
      actual Phase-13-style design + scaffold effort, using its
      design-phase checklist above as the starting task list) once
      `tpt-kinetix-lean` reaches a stable v1 — **realtime chosen (2026-08-13)**:
      design doc `docs/realtime-codec-design.md` written (all 7 DECISION blocks
      resolved), and the scaffold is started — `tpt-kinetix-bitstream` (shared
      BitReader + rANS) + `tpt-kinetix-realtime` (profile-aware headers +
      decoder shell) created and compiling. See Phase 16.

## Phase 15 — `tpt-kinetix-vision`: Video-for-Machines Codec (design phase) (2026-07-20)

> Goal: an original codec design that optimizes rate-distortion for
> downstream detector/classifier/embedding-model accuracy per bit, rather
> than human perceptual quality. Backlog item promoted to a design phase per
> the Phase 14 rule (design section required before implementation starts).
> Full rationale in `docs/codec-backlog.md` ("Original Specialist Codec
> Concepts" table). Overlaps `tpt-kinetix-lean`'s embedded/edge-camera
> target but shares no bitstream design yet — track independently until the
> shared-primitives question below is resolved.

### Design (resolved — see `docs/vision-codec-design.md`)
- [x] Decide the target consumer model class(es) for v1 — **Object detection
      (YOLO/DETR-family)** as v1 target, evolving to model-agnostic quantization
      later; documented as DECISION 1
- [x] Write the format design doc: header layout including a
      `chroma_present` flag (chroma is optional, not just subsampled —
      luma-only is the default for detection/pose/tracking encodes), and a
      bit-depth/quantization scheme matched to the target model's trained
      input precision rather than the human-eye 8-bit convention —
      **drafted in `docs/vision-codec-design.md`; all 8 decision points
      resolved (chroma handling, bit depth, quant matrix, output contract,
      metrics, budget, Lean relationship, target platform)**
- [x] Design the decode path's primary output contract: a feature/embedding
      tensor (bitstream → tensor), with full pixel reconstruction as a
      secondary/on-demand path for human review of a flagged clip — **dual-path
      `VisionDecoder` trait with `decode_tensor()` + `decode_pixels()`,
      `Tensor` output type; see DECISION 5**
- [x] Decide how "detector/classifier accuracy per bit" is measured for
      design validation — **mAP-vs-bitrate on COCO-val with YOLOv8-n as v1
      metric; see DECISION 6**
- [x] Document the memory/perf budget for v1 — **embedded envelope (RPi-class,
      ~20 MB arena, <10 ms/frame); see DECISION 7**
- [x] Decide the relationship to `tpt-kinetix-lean` — **start independent,
      extract later into `tpt-kinetix-bitstream` once both codecs are stable
      (DECISION 8)**

### Scaffold (completed)
- [x] Scaffold `tpt-kinetix-vision` crate from `templates/codec-crate/`,
      add to workspace `members` — `tpt-kinetix-vision/Cargo.toml`,
      `src/lib.rs`, `README.md` created; `Tensor` type and `VisionDecoder`
      trait (dual-path `decode_tensor` + `decode_pixels`) implemented as
      scaffold with the honesty contract (`pixel_exact: false`, strict mode
      returns `NotPixelExact`)

## Phase 16 — Granular Next Steps (2026-08-06)

> Session broke the P-frame CAVLC desync investigation out of the single
> `[-]` blocker in Phase 12 C.1 and into concrete, individually-verifiable
> sub-tasks. The deep codec items (full CABAC, AV1 reconstruction, crates.io
> publish with token) remain large; the items here are the tractable ones that
> can be landed without a per-block CAVLC oracle or a crates.io credential.

### Codec-correctness (small, verifiable)
- [x] Clean up all debug `eprintln!`s added during the P-frame investigation
      (`slice_data.rs`, `decoder.rs`, `cavlc_tables.rs`) and restore clean
      `parse_cavlc_block`. NOTE (2026-08-07): the `decoder.rs` debug `eprintln!`s
      (`[DEBUG] try_decode_real_slice …`, `P-path: parse error`) were in fact
      still present and have now been removed; `cargo build` is warning-free.
- [x] Keep the `dec_ref_pic_marking` slice-header fix: it is now driven by
      `nal_ref_idc` (not `slice_type`) in `slice::parse_with_context`, and
      `parse_with_context` takes the new `nal_ref_idc` argument. This was the
      most likely candidate root-cause for the P-slice (non-reference) desync.
- [x] Write a minimal, self-contained P-frame CAVLC characterization test
      (`tests/p_slice_cavlc_invariant.rs`): generates a 2-frame IP clip with
      `ffmpeg`, extracts the P-slice NAL, and calls `parse_p_slice` **directly**
       (bypassing the decoder's skip-frame fallback). It now PASSES — the
       Phase 12 C.1 desync is resolved (the slice parses to completion). It
       serves as the regression guard for the CAVLC P-slice parse path.
- [x] Pin the exact P-frame residual off-by-one with a CAVLC oracle. **RESOLVED
        2026-08-08 (Phase C.2) — verified 2026-08-13.** The throwaway
        independent oracle (`tests/p_slice_oracle2.rs`, route (b): a fresh
        §9.2.2 level-assembly re-implementation sharing only the ffmpeg-verified
        VLC tables, walked over the exact 64×48 IP P-slice bytes and diffed
        per-block against `parse_cavlc_block`'s traced coeffs) reports
        **0 mismatches across all 104 luma/chroma-AC/chroma-DC blocks**. So the
        residual decode path is correct and no off-by-one exists in
        `level`/`run_before`/`total_zeros` assembly. The earlier `max_diff=2`
        over 49/4608 samples was a **false premise in the test harness**, not a
        decoder bug: `-x264-params deblock=0` only zeroes x264's alpha/beta
        offset and does *not* set `disable_deblocking_filter_idc=1`, so the
        conformance test was comparing against an ffmpeg reference with the
        in-loop deblocking **on** while assuming it off. With deblocking
        genuinely disabled (`no-deblock=1`) the P-frame decode is bit-exact
        (`max_diff=0`), and with it enabled the per-4×4-block `bS` deblocking
        fix (Phase C.2) also makes it bit-exact. The strict `max_diff=0`
        assertion in `p_frame_conformance` is live and passing.
- [x] Once pinned, fix the offending routine — **nothing to fix**. The oracle
        confirmed the residual decode was never the bug; the real (and already
        fixed) gap was the deblocking `bS` per-4×4-block derivation in
        `deblock.rs` (Phase C.2). The strict `max_diff=0` assertion in
        `p_frame_conformance` is re-enabled and passing (both deblocking on/off).

### Publishing (tractable, no credential needed)
- [x] `cargo publish --dry-run` revealed `tpt-kinetix-core@0.1.0` **already
      exists on crates.io** — so the real publish (Phases 8/10) has been done by
      a maintainer already; the remaining step is re-publish after version bump.
- [x] Fixed the publish blocker: internal crate deps in `[workspace.dependencies]`
      and the inline `tpt-kinetix-test-utils` dev-deps lacked a `version`
      requirement, which crates.io rejects (`all dependencies must have a version
      requirement specified when publishing`). Added `version = "0.1.0"` to all
      internal `path` deps. Dry-run now fails only on the *uncommitted-changes*
      guard (expected during dev) rather than the version error — i.e. the
      packaging gate is cleared. A maintainer must commit, bump versions, and run
      the real `cargo publish` with a token.
 - [x] Run `cargo package --list` per crate to confirm no stray files are
       packaged — verified with `cargo package --list --workspace --allow-dirty`;
       only intended test/fuzz corpora (`tests/corpus/mp4/*.bin`,
       `fuzz_*_input.bin`) would ship, no build artifacts (`target/`, `.git/`,
       `.exe`, `.wasm`, or media samples).

### Status notes
- The P-frame CAVLC **bit-position desync is resolved** (`p_slice_cavlc_invariant`
   passes). The once-suspected **residual precision issue is also resolved**:
   the independent CAVLC oracle (`tests/p_slice_oracle2.rs`) finds **0
   mismatches** across all 104 blocks of the 64×48 IP clip, and
   `p_frame_conformance` decodes **bit-exact (`max_diff=0`)** both with
   deblocking disabled (`no-deblock=1`) and enabled. The earlier `max_diff=2`
   over 49/4608 samples was a **test-harness false premise**, not a decoder bug:
   `-x264-params deblock=0` only zeroes x264's alpha/beta offset and does not
   set `disable_deblocking_filter_idc=1`, so the conformance test was comparing
   against an ffmpeg reference with the in-loop deblocking **on** while assuming
   it off. The real fix (Phase C.2) was the per-4×4-block deblocking `bS`
   derivation in `deblock.rs`. All P-slice MVs are (0,0)/ref 0, so MC/reference
   copy is exercised and correct; `coeff_token`, `total_zeros`, and
   `run_before` tables match FFmpeg `h264data.h` exactly, and the I-frame path
   (shared residual math) is bit-exact — confirming the residual decode has no
   off-by-one.
- `crates.io` real publish (Phases 8/10) is **not** performed: it needs a
  crates.io token and network access and must be done deliberately by a
  maintainer.

## Phase 17 — Codec Conformance & Benchmark Reporting (2026-08-07)

> Source: session plan `judt-thinking-about-all-moonlit-flute` (prompted by
> "can we test that these codecs work / benchmark them against ffmpeg?").
> Ground truth established this session: `tpt-kinetix-test-utils` already has
> real ffmpeg-comparison plumbing (`reference.rs`, `pixel_diff.rs`,
> `synthetic.rs`) and ~20 ffmpeg-gated tests exist across the workspace, but
> (a) there is no single command that reports per-codec status, and (b) CI
> never installs `ffmpeg`/`dav1d`, so every gated test silently skips there —
> CI has been "green" without ever running a real comparison. Likewise, the 3
> existing criterion benches report per-crate with no single glanceable
> cross-codec table. This phase does **not** touch codec-correctness work
> (H.264 CABAC/P-frame residual, AV1 entropy decoder — tracked in Phase 12);
> it makes existing per-codec maturity queryable in one command and
> CI-enforced (correctness), adds one unified cross-codec performance table
> (speed), and fills two measurement gaps along the way (AAC benchmark; AV1
> encode size/quality vs ffmpeg). Refined mid-session per user ask: benchmark
> results should be presented as a side-by-side table, not just criterion's
> own per-crate HTML reports — added as a new standalone `bench_report` tool
> (§ below) rather than a criterion-JSON scraper (criterion's
> `target/criterion/*/base/estimates.json` is undocumented/unstable, not
> worth building a parser against).

### Status reporting tool
- [x] Add `tpt-kinetix-test-utils/src/audio_diff.rs` (`pcm_within_tolerance`,
      `pcm_max_abs_diff`, `pcm_diff_count`) and wire `pub mod audio_diff;` into
      `lib.rs` — implemented 2026-08-12
- [x] Add `reference::decode_av1_with_ffmpeg(obu, w, h)` and
      `reference::decode_aac_with_ffmpeg(adts)` to
      `tpt-kinetix-test-utils/src/reference.rs` (ffmpeg `-f obu` / `-f adts`
      round trips — no standalone `dav1d` CLI needed; `decode_aac_with_ffmpeg`
      parses sample rate/channels from the ADTS header and returns per-1024-block
      `f32` `AudioFrame`s) — implemented 2026-08-12
- [x] Add `synthetic::generate_h264_cavlc_iframe_clip` and
      `generate_h264_cavlc_ip_clip` to `synthetic.rs` (ported from the
      `generate()` helpers in `cavlc_conformance.rs` / `p_frame_conformance.rs`
      as new ffmpeg-stdout-streaming functions — leave those two test files
      untouched) — implemented 2026-08-12
- [x] Add `tpt-kinetix-aac`, `tpt-kinetix-lean`, `tpt-kinetix-vision` as
      `tpt-kinetix-test-utils` dev-dependencies (also surfaced in the
      `codec_status` example, which now prints all five decoders' capabilities)
      — implemented 2026-08-12
- [x] Add `tpt-kinetix-test-utils/examples/codec_status.rs`: prints each
      codec's `DecoderCapabilities` (canonical machine-readable status) and
      exits 1 under `--strict` if any decoder is not `pixel_exact`. (Simpler
      than the markdown-table variant; pulls `tpt-kinetix-aac` into
      `tpt-kinetix-test-utils` dev-deps.)

### Benchmarks
 - [x] Add `tpt-kinetix-aac/benches/decode_throughput.rs` (criterion,
       mirrors `tpt-kinetix-h264/benches/decode_throughput.rs`; embeds a
       real ffmpeg-generated AAC-LC ADTS clip as bytes, so no runtime ffmpeg
       dependency) + `criterion` dev-dep and `[[bench]]` in
       `tpt-kinetix-aac/Cargo.toml`
 - [x] Extend `tpt-kinetix-av1/benches/av1_encode.rs`: extend the existing
       single `bench_function` into a 3-way `benchmark_group` (`kinetix`,
       `librav1e`, `libaom`) for wall-clock only — size/PSNR comparison lives
       in `bench_report` instead, not duplicated here; the ffmpeg comparison
       benches are guarded by `ffmpeg_available()`
- [x] Add `tpt-kinetix-test-utils/examples/bench_report.rs`: standalone tool that
      runs every Criterion bench (via `cargo bench`) and prints a consolidated
      `time:` report for h264/av1/pipeline; accepts an optional crate list and
      `--release`.

### Discoverability & CI
- [x] Add `tpt-kinetix-h264/examples/gen_corpus.rs` (writes a small
      `testsrc_WxH.h264` corpus into `$TMPDIR/h264_corpus`, same ffmpeg
      pattern as `cavlc_conformance.rs::generate()`) so the existing but
      undiscoverable `corpus_check.rs` example is runnable with one command
- [x] Add `justfile` recipes: `conformance` (runs `codec_status`),
      `corpus-check` (runs `gen_corpus` then `corpus_check`), `bench` (runs
      the h264/av1/pipeline criterion benches), `bench-report`
      (runs `bench_report` in `--release`)
 - [x] Add a new `conformance` job to `.github/workflows/ci.yml` (ubuntu-only):
       installs ffmpeg via `apt-get`, installs cargo-nextest, runs
       `cargo nextest run -p tpt-kinetix-h264 -p tpt-kinetix-aac
       -p tpt-kinetix-av1 -p tpt-kinetix-stream -p tpt-kinetix-test-utils`
       (no exclusion needed — `cavlc_pframe_no_deblock_is_bitexact` now PASSES,
       fixed in Phase C.2; all ffmpeg-gated conformance tests in those packages
       were verified green locally), then prints `codec_status`. The strict
       `codec_status -- --strict` assertion is included as a `continue-on-error`
       non-blocking step because h264/av1 CABAC paths are not yet pixel_exact;
       it becomes the real gate once those decoders reach pixel_exact.
- [x] Fix the stale `tpt-kinetix-h264/README.md` "Status & known
      limitations" section (still claimed skip-only placeholder macroblocks
      and lists intra prediction/deblocking as unimplemented, contradicting
      the actual bit-exact-I-frame state from Phase 12); point it at
      `just conformance` / `just bench-report` as the living source of truth

### Verification
- [x] `cargo build --workspace` — full workspace compiles (root `Cargo.toml`
      `workspace.dependencies` gained `tpt-kinetix-lean`/`tpt-kinetix-vision`)
- [x] `cargo test -p tpt-kinetix-test-utils --lib` — 17 passed
- [x] `cargo test -p tpt-kinetix-test-utils --test conformance` — 7 passed,
      including the new `aac_vs_ffmpeg_reference_pcm_when_available` which
      exercises `decode_aac_with_ffmpeg` + `audio_diff` against real `ffmpeg`
      PCM (AAC-LC within 1e-2 tolerance)
- [x] `cargo clippy -p tpt-kinetix-test-utils --all-targets` — clean (only
      pre-existing `tpt-kinetix-h264` warnings, unrelated to this work)
- [~] `just conformance` / `just bench-report` — verified the underlying
      commands directly (`cargo run -p tpt-kinetix-test-utils --example
      codec_status` prints all five decoders; `bench_report` example compiles);
      `just` itself cannot spawn `sh` on this Windows host so the recipes were
      not invoked via `just`. CI `conformance` job's nextest filter (Phase 17
      §Discoverability) remains to be re-verified locally before push.
## Phase 16 — `tpt-kinetix-realtime`: Realtime Codec (scaffold + implementation) (2026-08-13)

> Goal: an original codec whose design center is **sub-frame latency** and
> **graceful degradation under packet loss** (not max compression ratio).
> Promoted from the Phase 14 backlog per the Phase 14 rule: the design-phase
> checklist is fully resolved and `docs/realtime-codec-design.md` is written
> (7 DECISION blocks: hybrid loss recovery, rolling-intra + on-demand-IDR GOP,
> fixed slice grid, encode-deadline/decode-bounded-work contract,
> packet-loss-vs-quality + stall-rate validation, profile-parameterized v1
> budget, and extraction of a shared `tpt-kinetix-bitstream` crate).
>
> **Profile decision:** profile-agnostic — cloud gaming / conferencing / AR
> are three preset parameter sets over one shared bitstream, not separate
> codecs. AR is the hardest preset (leans on `tpt-kinetix-lean`'s power
> primitives). See the design doc for the full rationale.

### Scaffold (done 2026-08-13)

- [x] Extract `tpt-kinetix-bitstream` from lean (DECISION 7) — `BitReader` +
      rANS (`RansEncoder`/`RansDecoder`/`RansStreamSet`/`SymbolModel`/
      `StaticModel`) copied verbatim and now the single source of truth;
      `tpt-kinetix-lean` and `tpt-kinetix-vision` still carry their own copies
      (migration to depend on `tpt-kinetix-bitstream` is the immediate
      follow-up — see Phase 16 item below). Crate compiles + clippy-clean,
      11 tests pass.
- [x] Create `tpt-kinetix-realtime` crate: `Cargo.toml` (deps on `core` +
      `bitstream`), `lib.rs` (profile-agnostic module docs), `headers.rs`
      (profile-aware `SequenceHeader`/`FrameHeader`: `ProfilePreset` enum,
      slice-grid, FEC overhead, foveation flag, intra-refresh mask,
      `deadline_ms`, `force_idr`; parse + to_bytes + validation, round-trips),
      `decoder.rs` (`RealtimeDecoder` shell reporting `pixel_exact: false`
      per the honesty contract). Wired into workspace `Cargo.toml` (members +
      `workspace.dependencies`). Compiles + clippy-clean, 11 tests pass.

### Implementation (remaining, per design doc "Implementation order")

- [x] Migrate `tpt-kinetix-lean` + `tpt-kinetix-vision` to depend on
      `tpt-kinetix-bitstream` (delete their now-duplicated `bitreader.rs` /
      `rans.rs`); true "extract now" completion of DECISION 7 — **done for
      lean (2026-08-13)**: added `tpt-kinetix-bitstream` dep, repointed
      `headers.rs`/`decoder.rs`/`lib.rs` doc links at it, deleted
      `lean/src/bitreader.rs` + `lean/src/rans.rs`, and updated the fuzz
      target (`tpt_kinetix_lean::bitreader::BitReader` →
      `tpt_kinetix_bitstream::BitReader`). **vision had no duplicated
      primitives** — its `src` is a single decode-shell `lib.rs` with no
      `BitReader`/`rANS` code — so there was nothing to migrate; vision will
      depend on `tpt-kinetix-bitstream` when its reconstruction is
      implemented.
- [x] Port lean's intra + unidirectional-P reconstruction into realtime
       (DECISION 2) — **done.** `tpt-kinetix-realtime` has its own
       reconstruction path (`prediction.rs`/`transform.rs`/`deblock.rs`/
       `reconstruct.rs`) wired end-to-end into `RealtimeDecoder::decode`. The
       five tests the 2026-08-14 session note flagged as failing
       (`transform::tests::dct_round_trip_4/8/16`,
       `transform::tests::hadamard_round_trip`,
       `reconstruct::tests::keyframe_round_trips_at_qp0`) now pass — the
       integer Walsh–Hadamard transform bank is exactly invertible, so the
       `qp == 0` lossless round-trip holds. Realtime is an original codec with
       no external reference oracle, so `pixel_exact` stays `false` per the
       honesty contract (see `decoder.rs`). Flipped `[~] → [x]` 2026-08-15.
- [x] Add slice-grid framing: each slice = one independent rANS sub-stream,
      self-contained (DECISION 3); wire `RansStreamSet` per-frame — **done
      (2026-08-13)**: `src/slice.rs` `SliceGrid` frames/unframes the
      `cols*rows` slice payloads via `tpt_kinetix_bitstream::RansStreamSet`,
      with count validation and the 255-sub-stream cap.
- [x] Add intra-refresh masking (`refresh_mask`) + on-demand `force_idr`
      resync path (DECISION 2) — **done (2026-08-13)**: `src/refresh.rs`
      `IntraRefreshScheduler` computes the per-frame `intra_refresh_mask`
      bitmask (cycling `ceil(rows/period)` rows/frame, wrapping, covering all
      rows within `period`); `force_idr` is already a frame-header field.
- [x] Add FEC packet framing (RaptorQ or RS) + decoder concealment as the
      terminal fallback (DECISION 1) — **done (2026-08-13)**: `src/fec.rs`
      `Fec` is a systematic XOR erasure coder over the framed frame payload
      (fixed 256-byte source symbols), emitting `repair_count` parity symbols
      (one per round-robin group) that recover up to one loss per group; losses
      beyond that fall through to intra-refresh/concealment as the hybrid
      design intends (RS/RaptorQ noted as the v2 MDS upgrade). `src/conceal.rs`
      `conceal()` does temporal concealment (reuse previous frame's slice) so
      the decoder never stalls. Both round-trip tested (29 realtime tests pass).
- [x] Add `deadline_ms` encode-side rate-control hook + `max_decode_ms`
      decoder capability (DECISION 4) — `src/rate.rs` implements
      `max_decode_ms_estimate` (turns a `SequenceHeader` into the decoder's
      `max_decode_ms`), `adapt_to_deadline` (encode-side `RateControlAction`
      from `deadline_ms`/`elapsed_ms`/`current_qp`), and `EncodeDeadline`;
      `headers.rs` carries `deadline_ms`/`max_deadline_ms` per frame/sequence
      (round-trip tested).
- [x] Build the packet-loss-vs-quality + stall-rate validation harness
       (DECISION 5) in `tpt-kinetix-test-utils` behind a `realtime-bench`
       feature — **done (2026-08-15).** `tpt-kinetix-test-utils/src/
       realtime_bench.rs` encodes a synthetic moving clip through the real
       realtime pipeline (`encode_frame_slices` → `SliceGrid → `Fec`),
       injects reproducible packet loss at a given rate, recovers via FEC, and
       falls back to temporal concealment for anything still missing; it then
       decodes with the real `RealtimeDecoder` and reports `mean_psnr_y_db` /
       `min_psnr_y_db` (quality half) and `stall_rate` (degradation half) per
       loss rate via `run_loss_curve`. At `qp == 0` a zero-loss run is
       lossless (PSNR = +inf, 0% stall), and 30% loss forces stalls —
       covered by 4 deterministic harness tests. The feature is default-off so
       it does not add to the normal `just check` wall time.
- [x] (AR profile) Add foveation / gaze-map support (DECISION 6) — **done.**
       `foveation.rs` implements `GazeMap` + `slice_qp_by_index` (foveal
       slices at `base_qp`, peripheral slices up to `base_qp +
       MAX_FOVEATION_QP_DELTA`, normalised by distance from the gaze centre),
       the sequence `foveation_enabled` flag and frame `foveation_center_*`
       fields carry it on the wire, the encoder/decoder both derive per-slice
       QP from the same header fields (round-trip exact), and the reconstruction
       path already calls `slice_qp_by_index`. Four unit tests cover the falloff.

## Phase 15 — `tpt-kinetix-volumetric` (design phase, 2026-08-13)

> Source: prioritized original specialist codec from `docs/codec-backlog.md`
> (`tpt-kinetix-volumetric` — point-cloud / volumetric / AR-VR content;
> 2D video codecs don't apply). Design doc: `docs/volumetric-codec-design.md`.
> Each checklist item maps to a `DECISION:` block in that doc.
>
> **Updated:** all 8 design decisions are resolved and pre-scaffold work is
> substantially implemented — header parsing (`src/header.rs`), octree
> geometry decode (`src/octree.rs`), lift/RAHT attribute decode
> (`src/attribute.rs`), a TMC13-oracle conformance harness, and a
> `cargo-fuzz` target all exist and round-trip/pass today. What remains is
> the direct Kinetix-vs-TMC13 bit-exact cross-check (pending coding-tool
> alignment) — until that lands, the decoder still reports
> `pixel_exact: false` and strict mode rejects its output.

### Design decisions
- [x] Decide the target volumetric representation for v1 (point cloud vs.
      voxel grid vs. mesh+texture) — **DECISION 1 resolved: point cloud**
      (see `docs/volumetric-codec-design.md`; voxel/mesh deferred to v2 as
      output representations derived from the decoded cloud)
- [x] Decide the geometry coding method for the v1 point cloud (octree vs.
      predictive vs. trisoup) — **DECISION 2 resolved: octree** (context-
      modeled occupancy bitmap per node; predictive trisoup deferred to v2,
      see `docs/volumetric-codec-design.md`)
- [x] Decide the attribute coding method (RAHT vs. region-adaptive
      predictive/lift vs. direct) — **DECISION 3 resolved: both normative,
      lift primary / RAHT selectable** (see `docs/volumetric-codec-design.md`)
- [x] Decide attribute bit depth + lossless vs. lossy mode — **DECISION 4
      resolved: per-attribute bit depth (8-bit default, 10–16-bit HDR),
      lossless + lossy via shared quantizer; strict mode rejects lossy**
- [x] Decide static-single-cloud (v1) vs. dynamic inter-frame prediction —
      **DECISION 5 resolved: static for v1**, `dynamic` flag reserved →
      `Unsupported` on v1 decoder
- [x] Decide bitstream alignment: G-PCC/V-PCC-faithful core with Kinetix
      framing (recommended) vs. pure original bitstream — **DECISION 6
      resolved: (C) G-PCC-faithful core, Kinetix framing** (`magic b"VOLU"`)
- [x] Decide relationship to `tpt-kinetix-bitstream` (rANS), the 2D codecs
      (V-PCC projection path), and the `PointCloud` core output type —
      **DECISION 7 resolved: depends on core + bitstream only; `PointCloud`
      output type in core; V-PCC is optional feature-gated dep**
- [x] Set memory/perf budget + conformance oracle (MPEG-I G-PCC TMC13) for
      v1 — **DECISION 8 resolved: 10M-point cap, ~64 MB arena, TMC13 oracle**

### Pre-scaffold (after decisions resolve)
- [x] Add `tpt-kinetix-volumetric` to workspace `Cargo.toml` members
      (design-phase only — not added to `release-plz.toml` publish list,
      matching `tpt-kinetix-vision`/`lean`/`screen`)
- [x] Add `PointCloud` decoded-output type to `tpt-kinetix-core`
      (`frame.rs`: `PointCloud` + `PointAttribute` + `PointAttributeKind`)
- [x] Implement sequence/frame header parsing (DECISION 6 framing) — `src/header.rs`: byte-aligned `b"VOLU"` sequence + frame headers, rejects dynamic/reserved/over-cap streams
 - [x] Implement octree geometry decode (DECISION 2) — `src/octree.rs`: MSB-first
       context-modeled occupancy octree via per-context rANS models; reconstructs
       integer coords from the descent path, round-trips against the in-crate encoder
 - [x] Implement attribute lift/RAHT decode (DECISION 3) — `src/attribute.rs`: lift
       (k-neighbour predictive + residual, lossless when quant step = 1) and RAHT
       (lossless integer Haar over the Morton-ordered stream); both round-trip
 - [x] Build a TMC13-oracle conformance harness (DECISION 8) — `tpt-kinetix-test-utils`:
       `src/tmc13.rs` drives `tmc3` (gated, skips when absent) + `tests/volumetric_conformance.rs`;
       direct Kinetix-vs-TMC13 bit-exact cross-check pending coding-tool alignment
       (decoder still reports `pixel_exact: false`; strict mode rejects output)
 - [x] Add `cargo-fuzz` target for the header + octree parser — `fuzz/` (`fuzz_volumetric_parser`),
       exercises full decode path (header → octree → attributes) panic-free on any input

