# `tpt-kinetix-volumetric` — Design Draft

> **Status:** Design decisions **all resolved**. The original eight `DECISION:`
> blocks were resolved 2026-08-13; **DECISION 9** (compression-efficiency
> measurement / Draco + TMC13 baselines) and **DECISION 10** (explicit
> shared-primitive reconciliation — the `todo.md` checklist item 6 "shares no
> primitives" statement) were added 2026-08-15 to close the two checklist items
> that had no prior `DECISION`. The crate may now enter the pre-scaffold
> checklist in `todo.md` (Phase 15).
>
> This doc is the home for the `tpt-kinetix-volumetric` design-phase checklist
> in `todo.md` (the volumetric design phase). Each checklist item maps to a
> `DECISION:` block here.

### todo.md design-phase checklist — item → decision map

The six-item checklist ("`tpt-kinetix-volumetric` — design-phase checklist,
start here once prioritized") maps onto the `DECISION:` blocks below. Items 4
(compression-efficiency measurement) and 6 (explicit shared-primitive
statement) had no dedicated `DECISION` before this revision and are resolved
here as **DECISION 9** and **DECISION 10** respectively.

| # | Checklist item | Resolved by |
|---|---|---|
| 1 | Target representation (point cloud / voxel grid / mesh+texture) | DECISION 1 |
| 2 | Reuse `tpt-kinetix-core` frame/packet types or new core types entirely | DECISION 7 + "Core-type reuse (item 2)" note below |
| 3 | Spatial-partitioning + entropy coding (e.g. octree) | DECISION 2 (octree geometry) + DECISION 3 (attribute) + DECISION 7 (rANS) |
| 4 | How compression efficiency is measured (Draco / MPEG PCC baseline) | DECISION 9 (new) |
| 5 | Memory / perf budget for v1 | DECISION 8 |
| 6 | Confirm it shares no primitives with other codecs (stated, not assumed) | DECISION 10 (new) |

#### Core-type reuse (checklist item 2)

This is explicit rather than assumed. The v1 point cloud is a **new** decoded
output type added to `tpt-kinetix-core` — `PointCloud` (`frame.rs`), parallel
to `VideoFrame`/`AudioFrame`. It does **not** reuse `VideoFrame`, `AudioFrame`,
or `PixelFormat`: those are 2D-lattice abstractions (fixed-width plane array,
pixel-format-driven layout) and have no meaning for an unstructured ℝ³ point
set. What *is* reused from core is only the cross-cutting contract plumbing
shared by every codec:

- `KinetixError` / `DecoderCapabilities` — the same error + capability
  introspection the 2D decoders expose, so the CLI / pipeline can detect an
  incomplete path (`NotPixelExact` analog: volumetric reports
  `pixel_exact = false` until the Kinetix-vs-TMC13 bit-exact cross-check
  passes).
- `Packet` / `Timestamp` — *only* as a thin transport envelope if a cloud is
  carried inside a container/demux path; they never describe the decoded
  representation itself.

So the representation is new core types entirely; the shared surface is the
codec-agnostic error/capability/transport infrastructure, not any frame or
sample data type.

## Goal

A codec for **volumetric / AR-VR content** — captured 3D scenes and objects
that exist in a 3D coordinate space rather than as a 2D frame grid. The data
shape is fundamentally different from everything else in this workspace: there
is no fixed-width pixel array, no 2D block partition that maps to spatial
neighbours the way H.264/AV1 blocks do, and the "frame" notion is a point set
(or occupancy field) in ℝ³.

The design follows the same from-scratch, spec-transcribing ethos as the rest
of the project (Phase 12: real normative tables, conformance harness vs. a
reference implementation, fuzz targets on every parser). The reference for a
point-cloud codec is the MPEG-I **G-PCC** reference software (TMC13) for
geometry/attribute coding, and **V-PCC** (TMC2) where a 2D-projection path is
chosen — both are permissively licensed and give us a bit-exact oracle the way
FFmpeg/`dav1d` do for the 2D codecs.

### Why not just use AV1/HEVC?

The 2D codecs in this workspace optimize a 2D lattice of samples. Volumetric
content is not a 2D lattice:

- A point cloud is an *unstructured* set of (x, y, z) positions with per-point
  attributes (colour, reflectance, normal). There is no natural 2D tiling.
- A voxel grid is a 3D lattice, not a 2D one; 3D neighbourhood structure and
  occupancy sparsity change the entire entropy-coding problem.
- AR/VR capture pipelines emit point clouds (Depthkit, Microsoft Volumetric,
  8i, LiDAR/depth fusion), not 2D frames with depth side-channels.

Existing 2D tools don't generalize: V-PCC *wraps* a 2D codec around a
projection of the cloud, but the projection/geometry layer itself is new
work. So this is a genuinely separate crate, not a profile of `tpt-kinetix-av1`.

---

## DECISION 1: Target volumetric representation for v1

This is the foundational decision — it determines the geometry coder, the
attribute coder, the memory model, and the conformance reference. The three
candidate representations:

| Alternative | Tradeoff |
|---|---|
| **A) Point cloud** | The dominant representation for "volumetric video" / AR-VR capture. Mature open standards (MPEG-I Part 5 G-PCC for static + dynamically-acquired point clouds; Part 9 V-PCC for dense video-derived clouds) give us spec tables to transcribe and a reference oracle (TMC13 / TMC2). Handles arbitrary topology and both static and animated content. Most general: voxel grids and meshes can be *derived/exported* from a point cloud. Unstructured neighbour structure is the only real cost, and G-PCC already solves it (octree + k-d tree sorting). |
| **B) Voxel grid** | A regular 3D lattice of occupancy + attributes. Simple addressing and 3D neighbourhoods (natural for SDF / occupancy / neural volumetric). But memory is O(N³) in resolution — a 1024³ grid is ~1 bn cells, intractable at useful fidelity. Best suited to a narrower niche (medical, scientific, neural fields), not AR-VR capture. A point-cloud codec can *emit* a voxel grid on demand; the reverse is lossy and lossy-information. |
| **C) Mesh + texture** | Traditional 3D-model coding (glTF/Draco/KTX). Well served by existing tooling and not what "volumetric video" of *captured* reality means. Animating captured meshes needs rigging/skinning/blendshapes that are out of scope for v1 and a poor fit for point-captured dynamic content. |

**Recommendation:** **(A) point cloud** for v1.

Rationale, in priority order:

1. **It is what the target use case produces.** AR-VR volumetric video is
   point-cloud sequences. Building for the representation the capture pipeline
   already emits avoids a forced conversion step and its information loss.
2. **It gives us a spec + oracle, matching project methodology.** G-PCC
   (TMC13) is an openly-licensed reference we can transcribe tables from and
   diff bit-exact against — exactly the Phase 12 workflow already proven for
   H.264/AV1. V-PCC (TMC2) additionally lets us reuse `tpt-kinetix-h264` /
   `tpt-kinetix-av1` as the 2D layer for a projection-based variant.
3. **It is the most general of the three.** Voxel grids and meshes are
   *sinks* that can be generated from a point cloud; starting there keeps the
   v1 data model open to those as later output representations without
   committing v1 to their memory/structure cost.
4. **Voxel grids (B) and meshes (C) become well-defined v2 representations**
   layered on top of the point-cloud core, rather than competing v1 targets.

### What "point cloud for v1" commits us to (scope boundaries)

- **Geometry** is a set of 3D positions. v1 codes geometry lossily-or-losslessly
  via an octree (G-PCC geometry octree) — the most general and the one TMC13
  uses by default. (Trisoup / predictive are DECISION 2.)
- **Attributes** (colour, optionally reflectance/normal) ride on the geometry's
  neighbour structure. v1 uses the region-adaptive hierarchical transform /
  predictive lift from G-PCC attribute coding. (RAHT vs lift is DECISION 3.)
- **Static-first.** v1 targets a single static cloud (or independently-coded
  frames). Inter-frame / dynamic-cloud prediction is deferred to a follow-up
  (DECISION 5) — G-PCC supports it, but it is not required to validate the
  core geometry+attribute path.
- **No mesh/voxel output in v1** beyond an optional export helper that
  *consumes* decoded points. The decoded output type is a `PointCloud`.

---

## DECISION 2: Geometry coding method (point cloud)

| Alternative | Tradeoff |
|---|---|
| **A) Octree (G-PCC default)** | Recursively subdivide space; code occupancy of 8 child nodes per level. Most general, handles arbitrary density, directly gives the neighbour ordering attribute coding needs. Our recommendation for v1. |
| **B) Predictive / octree-with-prediction** | Predict each point from previously decoded neighbours, code residuals. Better rate for dense, smooth clouds but needs a stable prediction context — builds on (A). |
| **C) Trisoup** | Surface-mesh extraction inside leaf nodes for smooth surfaces. Best for man-made/smooth objects, poor for sparse/noisy capture. A later refinement, not v1. |

**Recommendation:** **(A) octree** for v1, structured so (B) prediction can be
added as an in-octree refinement pass later.

**Resolved (2026-08-13): (A) octree for v1.** Details that this commits us to:

- **Occupancy coding per node.** At each octree level, the 8 child positions
  are coded as an occupancy bitmap (1 bit per child that is occupied), then
  the occupied children are visited in a fixed order. This is G-PCC's default
  geometry octree and is what TMC13 uses, so our conformance oracle diffs
  bit-exact against it.
- **Context-modeled occupancy.** The occupancy bitmap is entropy-coded with a
  context derived from the *neighbouring* node occupancies (the 6-face /
  12-edge / 8-corner neighbour pattern, as G-PCC's `neighPattern`). This is
  where the rate lives and where the `tpt-kinetix-bitstream` rANS context
  model plugs in.
- **Position precision.** Each point's final intra-leaf position is coded at
  the leaf resolution (the octree depth sets this). 8-bit intra-leaf position
  per axis is the G-PCC default; the sequence header carries octree depth so
  precision is negotiable without a format change.
- **Neighbour order is a side output, not just geometry.** The octree visit
  order (Morton / k-d sorted) is exactly the ordered point list that DECISION
  3's attribute coder consumes. Resolving geometry first is a precondition for
  resolving attribute coding — which is why DECISION 3 is the next item.

**Deferred to v2 (do NOT block v1):**
- **(B) Predictive octree** — predict each node/point from already-decoded
  neighbours and code residuals. Implemented as an *optional* refinement flag
  in the sequence header; the octree scaffolding above is built so this layers
  on without restructuring.
- **(C) Trisoup** — surface-mesh extraction in leaf nodes; only valuable for
  smooth man-made surfaces, poor for sparse/noisy capture. Out of scope for v1.

---

## DECISION 3: Attribute coding method

| Alternative | Tradeoff |
|---|---|
| **A) Region-Adaptive Hierarchical Transform (RAHT)** | Orthonormal 3D transform over the octree-indexed points; emits a coarse-to-fine coefficient stream. Good for smooth colour, bit-exact spec in G-PCC. |
| **B) Region-Adaptive Predictive / Lift** | Predict attributes from neighbours in Morton/k-d order, lift residuals. Often better rate for high-frequency detail; the G-PCC "lift" mode. |
| **C) Direct (no transform)** | Code raw attribute deltas. Trivial, only as a test baseline. |

**Recommendation:** start with **(B) lift/predictive** as primary (best rate
for captured content) and keep **(A) RAHT** as a selectable alternative coded
in the sequence header — both are normative in G-PCC, so supporting the flag
is cheap once the neighbour graph exists.

**Resolved (2026-08-13): both (A) RAHT and (B) lift are normative; (B) lift is
the v1 default, (A) RAHT is sequence-header-selectable.** Details this commits
us to:

- **Lift / predictive (default).** For each point in the octree-sorted order
  (DECISION 2's output), predict its attributes from already-decoded neighbours
  (G-PCC's `k` nearest neighbours in the reconstructed set), then apply the
  lifting transform to the prediction residuals and quantize. This is the
  G-PCC "lift" attribute mode and generally gives the best rate for captured
  AR/VR content with high-frequency colour detail. The neighbour search reuses
  the same occupancy/neighbour structure the octree already built.
- **RAHT (selectable).** When the sequence header selects RAHT, attributes are
  transformed by the region-adaptive hierarchical transform over the
  octree-indexed points, emitting a coarse-to-fine coefficient stream that is
  then quantized and rANS-coded. Useful for smooth colour where the orthonormal
  transform outperforms prediction; kept as a flag so the same decoder handles
  both without a format change.
- **Quantization is the loss knob.** Both modes share one quantizer step per
  attribute (the `quantizationSteps`/scale in G-PCC). This single parameter is
  what makes a stream lossy; setting it to 1 (or using the lossless path) makes
  attributes bit-exact. DECISION 4 narrows the bit-depth + lossless question.
- **(C) Direct** is not a shipped mode — it stays only as a unit-test baseline
  to validate the prediction/lift plumbing against trivial coding.

Both (A) and (B) are normative in G-PCC, so conformance against TMC13 covers
whichever the test stream selects. The decoder branches on the sequence-header
flag; the rANS symbol models come from `tpt-kinetix-bitstream`.

---

## DECISION 4: Colour / attribute bit depth and loss mode

Mirrors the bit-depth question from the other codecs: AR/VR capture is usually
8-bit RGB, but HDR / scientific capture is 10–16 bit. v1 should carry an
attribute bit-depth field and support both lossless and lossy attribute coding
(the lossy step is the quantizer in the lift/RAHT path).

**Resolved (2026-08-13):** v1 carries a per-attribute bit-depth field
(default **8-bit RGB**, optional **10–16-bit** for HDR/scientific) and supports
**both lossless and lossy** attribute coding through a single quantizer step
shared by the lift and RAHT paths (DECISION 3):

- **Lossless:** quantizer step = 1 (or the dedicated lossless lift path), so
  decoded attributes are bit-exact vs. the source — required for the TMC13
  lossless conformance tier and for HDR/scientific capture where "close enough"
  is disqualifying (same motivator as `tpt-kinetix-lossless`).
- **Lossy:** quantizer step > 1 trades attribute fidelity for rate; the same
  code path as lossless, just with coarser quantization. The decoder signals
  the achieved mode via `DecoderCapabilities` (mirroring the
  `pixel_exact`/`NotPixelExact` contract the 2D decoders already expose), so a
  caller can reject lossy output in strict mode.
- **Bit depth is per-attribute, in the sequence header** — colour, reflectance,
  and normal each declare their own depth, so an 8-bit-colour cloud can still
  carry 16-bit depth/reflectance without forcing the whole stream wide.


---

## DECISION 5: Static vs. dynamic (volumetric video)

| Alternative | Tradeoff |
|---|---|
| **A) Static single cloud (v1)** | One point set per file. Simplest; validates geometry + attribute end-to-end and bit-exact vs TMC13. |
| **B) Dynamic, inter-frame predicted** | Sequence of clouds with motion/occupancy prediction between frames (G-PCC "dynamic" mode). The real "volumetric video" product, but needs a temporal predictor and DPB-like state. Defer to v2. |
| **C) Animated via transform only** | Per-frame rigid/affine transforms of a static base cloud. Cheap, covers some AR use cases, but not general captured animation. |

**Recommendation:** **(A) static for v1**, design the sequence header so (B)
can be layered without a format change.

**Resolved (2026-08-13): (A) static single cloud for v1.** The sequence header
carries a `dynamic` flag (default `0`); setting it to `1` is reserved for a
future v2 dynamic mode and must cause a v1 decoder to return
`KinetixError::Unsupported` rather than silently mis-decoding. This keeps the
format open to (B) inter-frame prediction and (C) transform-only animation
without committing v1 to a temporal predictor or DPB-like state. v1 validates
geometry + attribute end-to-end and bit-exact vs TMC13 on static clouds only.

---

## DECISION 6: Original bitstream vs. MPEG-I alignment

| Alternative | Tradeoff |
|---|---|
| **A) Align to G-PCC/V-PCC bitstream** | Reuse the real standard's syntax/tables; conformance oracle is the actual reference software; interop with the ecosystem. Cost: we follow their framing, not our own. |
| **B) Original bitstream (like `lean`/`vision`)** | Full control, can optimize for our engine's parallelism model. Cost: no external oracle, must build our own conformance corpus, and we lose ecosystem interop. |
| **C) G-PCC-faithful core, Kinetix framing** | Transcribe G-PCC's *coding tools and tables* (octree, lift, RAHT) but wrap them in our own container/header conventions (byte-aligned headers like `lean`, `tpt-kinetix-bitstream` rANS). Best of both: real, oracle-checkable tools, our packaging. |

**Recommendation:** **(C)** — same philosophy as the 2D codecs (transcribe
normative tables, own framing). Gives us a bit-exact oracle (TMC13) while
keeping crate-internal consistency with `tpt-kinetix-bitstream`.

**Resolved (2026-08-13): (C) G-PCC-faithful core, Kinetix framing.** We
transcribe G-PCC's normative coding tools and tables — octree occupancy
contexts (DECISION 2), lift and RAHT attribute transforms (DECISION 3), the
attribute quantizer (DECISION 4) — and wrap them in our own container/header
conventions: byte-aligned sequence/frame headers (like `lean`), rANS entropy
coding from `tpt-kinetix-bitstream`, and a `magic` (`b"VOLU"`) distinct from
the MPEG container. This gives a bit-exact oracle (TMC13) for conformance while
keeping the crate consistent with the rest of the workspace. The one place we
deliberately diverge from raw G-PCC framing is the transport envelope, not the
coding tools — so the reference software remains a valid oracle for the coded
portion.

---

## DECISION 7: Relationship to `tpt-kinetix-bitstream` and the 2D codecs

`DECISION 6 (C)` implies sharing primitives:

- **rANS entropy coding** comes from `tpt-kinetix-bitstream` (already
  extracted from `lean`). Geometry/attribute symbols code through it, mirroring
  how `vision` planned to reuse it.
- **V-PCC projection variant** (DECISION 1, alternative A's V-PCC path) would
  feed geometry patches through `tpt-kinetix-h264` / `tpt-kinetix-av1` as the
  2D layer — an explicit dependency *option*, gated behind a feature so the
  core point-cloud path has no 2D-codec dependency.
- **Output type:** a `PointCloud` struct (positions + attribute buffers) added
  to `tpt-kinetix-core` as the decoded output, parallel to `VideoFrame`.

**Resolved (2026-08-13):** the relationships are confirmed and pinned:

- **rANS entropy coding** comes from `tpt-kinetix-bitstream` (already
  extracted from `lean`). Geometry/attribute symbols code through it, mirroring
  how `vision` reuses it. No bitreader/rANS reimplementation in this crate.
- **V-PCC projection** (a future variant of DECISION 1's point-cloud path)
  is an *optional* feature-gated dependency on `tpt-kinetix-h264` /
  `tpt-kinetix-av1` as the 2D layer for geometry-patch projection. The core
  point-cloud (octree + lift/RAHT) path has **no** 2D-codec dependency, so the
  v1 crate depends only on `tpt-kinetix-core` + `tpt-kinetix-bitstream`.
- **Output type:** a `PointCloud` struct (positions + per-attribute buffers)
  added to `tpt-kinetix-core` as the decoded output, parallel to `VideoFrame`.
  It is the single public output of `VolumetricDecoder::decode()`; voxel/mesh
  conversion (DECISION 1) is a separate, later *consumer-side* helper, not part
  of the decode contract.

---

## DECISION 8: Memory / perf budget and conformance reference for v1

| Constraint | Value | Rationale |
|---|---|---|
| Max points per cloud | ~10M | Covers room-scale AR captures; bounds octree depth + arena |
| Max octree depth | 10–12 | ~sub-mm precision at room scale; configurable in header |
| Decode arena ceiling | ~64 MB | Position (3×f32) + colour (3–4×u8/u16) × 10M, plus workspace |
| Conformance oracle | MPEG-I G-PCC TMC13 reference software | Bit-exact geometry/attribute diff, same role as `ffmpeg`/`dav1d` |
| Platform | Server / edge-inference class (x86 / Jetson), not MCU | Point clouds are heavier than 2D frames; no `no_std` for v1 |

**Resolved (2026-08-13):** the budget above is the v1 envelope. The decode
arena is bounded by `max_points` declared in the sequence header (hard cap
10M) so a malformed stream cannot force unbounded allocation — the same
defensive ceiling the H.264 decoder applies via `MAX_MB_COUNT`. Conformance is
the **MPEG-I G-PCC TMC13 reference software**, run as an ffmpeg-gated external
oracle in `tpt-kinetix-test-utils` (bit-exact geometry + attribute diff),
exactly as the 2D codecs gate on `ffmpeg`/`dav1d`. All design decisions are
now resolved; the crate may enter scaffolding (Phase 15 pre-scaffold list).

---

## DECISION 9: Compression-efficiency measurement & baselines (checklist item 4)

A codec with no objective function cannot judge whether a design change is an
improvement. For a point cloud this is *not* PSNR on a 2D raster — the metric
space is different, which is the whole point of the "fundamentally different
data shape" flag. v1 validation measures two axes:

**Geometry fidelity**

| Metric | Definition | Use |
|---|---|---|
| **D1 PSNR** | Peak-Signal-to-Noise on point-to-point (nearest-neighbour) Euclidean distance | Primary geometry loss metric, matches how G-PCC reports geometry |
| **D2 PSNR** | PSNR on point-to-plane (Hausdorff / plane-distance) error | Better for surface-capture clouds; the second G-PCC geometry metric |
| **Voxel recall / precision** | Overlap of reconstructed vs. source occupancy on a fixed voxel grid | Invariant check that the cloud occupies the right volume |

**Attribute fidelity**

| Metric | Definition | Use |
|---|---|---|
| **Y/U/V or RGB PSNR** | Per-attribute channel PSNR on the decoded colour/reflectance | Color fidelity at the chosen bit depth |
| **Bitrate** | Total coded bytes ÷ `num_points` → **bits-per-point** | The rate denominator for any rate-distortion curve |

**Baselines the v1 design is validated against** (the "design validation"
requirement from the checklist):

1. **MPEG-I G-PCC reference (TMC13)** — the bit-exact oracle (DECISION 8). The
   primary baseline: at matched D1/D2 PSNR, Kinetix-coded bitrate should be
   within a small, documented delta of TMC13 (the "faithful core" of DECISION
   6 means we expect *parity*, not superiority — the win is our framing /
   parallelism / Rust-safety, not better compression).
2. **Google Draco** — the dominant open point-cloud/mesh codec, used as the
   *external* third-party baseline (not oracle). Draco is a different design
   center (mesh- and octree-geometry tuned for delivery/streaming), so the
   comparison documents where volumetric's G-PCC-derived tools win (rate at
   matched geometry for captured AR-VR clouds) vs. where Draco wins (small
   static meshes). This is the "baseline against Draco" the checklist asks for.

The harness lives in `tpt-kinetix-test-utils` as an ffmpeg-gated (here:
`tmc3`/`draco`/gated) external-oracle integration test, emitting a
rate–distortion table (bits-per-point vs. D1/D2 PSNR, vs. TMC13 and Draco)
saved as a test artifact — same gating philosophy as the 2D codecs' reference
harnesses, and the same role `cargo nextest` plays for the bit-exact tests.

**Resolved (this revision):** efficiency is measured by D1/D2 geometry PSNR +
per-attribute PSNR at **bits-per-point**, validated against **TMC13** (oracle
+ primary baseline) and **Draco** (external third-party baseline), with the
rate–distortion curve persisted as a test artifact. TMC13 parity is the target;
Draco is the benchmark that proves the design is competitive against a widely
deployed alternative.

---

## DECISION 10: Shared-primitive reconciliation — "shares no primitives" (checklist item 6)

`docs/codec-backlog.md` flags volumetric as "fundamentally different … 2D
video codecs don't apply at all" and the checklist asks to **confirm
explicitly** that it shares no primitives with any other kinetix codec —
deliberately *stated*, not assumed.

This requires splitting "primitive" into two distinct categories, because the
blanket "shares no primitives" from the backlog is **too broad as written**
and would be wrong if taken literally:

**A) Codec-domain primitives — NONE shared. (This is the real, correct claim.)**

Every algorithmic primitive that makes a codec *that codec* is unique to
volumetric and does not exist in any other crate:

- No 2D block/macroblock partition, no 8×8/16×16/64×64 transform bank, no
  intra prediction modes, no inter/MV prediction, no deblocking/CDEF/loop-
  restoration — none of the H.264/AV1/Lean/Vision machinery applies.
- No CABAC/CAVLC/2D-rANS-symbol-framing for a 2D lattice; the geometry
  coder is an **octree occupancy descent** (DECISION 2), an entirely different
  entropy structure.
- The attribute coder (RAHT / region-adaptive lift over a 3D neighbour graph,
  DECISION 3) shares nothing with 2D transform/quantization.
- No `VideoFrame`/`AudioFrame`/`PixelFormat` reuse (DECISION 7 + item 2 note).

So the *codec* shares **zero** primitives with any sibling crate. The
backlog's "fundamentally different data shape (3D, not 2D frames)" is exactly
right and is now a stated decision, not an assumption.

**B) Cross-cutting low-level infrastructure — DELIBERATELY shared.** This is
the correction to the backlog's blanket wording. Volumetric reuses the same
*engineering* primitives that `lean`, `vision`, `realtime`, `face`, `screen`,
and `lossless` already share:

- **`tpt-kinetix-bitstream`** — `BitReader` + rANS (`RansDecoder` /
  `RansStreamSet`) + the `SymbolModel` context-model trait. The octree
  occupancy bitmap (DECISION 2) and lift/RAHT residuals (DECISION 3) are
  entropy-coded *through* this crate, exactly as the other original codecs do.
  No bitreader/rANS is reimplemented here.
- **`tpt-kinetix-core`** — the `PointCloud` output type (DECISION 7), plus
  `KinetixError` / `DecoderCapabilities` / `Packet` / `Timestamp` contract
  plumbing (item 2 note).
- **Optional, feature-gated** V-PCC projection path could feed geometry
  patches through `tpt-kinetix-h264` / `tpt-kinetix-av1` as the 2D layer
  (DECISION 1/7) — an explicit *optional* dependency, never on the v1 core
  path.

**Resolved (this revision):** the stated decision is — *volumetric shares no
codec-domain primitives with any other kinetix codec (its geometry, attribute,
and entropy structure are unique to 3D point clouds), but it deliberately
reuses the shared `tpt-kinetix-bitstream` (rANS) + `tpt-kinetix-core`
(output-type / error / capability) infrastructure, consistent with every other
original codec in the workspace.* The backlog's "shares no primitives" is
therefore correct specifically for *codec* primitives and is now recorded as an
explicit, scoped decision rather than an unexamined assumption.

---

## Implementation order (post-design resolution)

1. Add `tpt-kinetix-volumetric` to the workspace `Cargo.toml` members (design-phase only — **not** added to the `release-plz.toml` publish list yet, matching `tpt-kinetix-vision`/`lean`/`screen`).
2. Add `PointCloud` output type to `tpt-kinetix-core`.
3. Port rANS primitives via `tpt-kinetix-bitstream` dependency.
4. Implement sequence/frame header parsing (DECISION 6 (C) framing).
5. Implement octree geometry decode (DECISION 2 (A)).
6. Implement attribute lift/RAHT decode (DECISION 3).
7. Build a TMC13-oracle conformance harness (DECISION 8).
8. Add `cargo-fuzz` target for the header + octree parser.
9. (Later) dynamic inter-frame prediction (DECISION 5 (B)).
