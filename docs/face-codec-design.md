# `tpt-kinetix-face` — Design Draft

> **Status:** Design phase — **all 8 design decisions resolved** (see checklist).
> The bitstream, representation, synthesizer, encoder, metric, budget, sibling-crate
> relationship, and honesty contract are settled; the crate is ready to be scaffolded
> from `templates/codec-crate/` once the open questions below are answered.
> This crate is an *original* bitstream design (like `tpt-kinetix-lean` and
> `tpt-kinetix-vision`), not a port of an existing standard.
>
> Backlog entry: `docs/codec-backlog.md` — *"Talking-head/video-conferencing
> via landmark-driven synthesis instead of pixel coding."*

---

## Goal

A face codec for the constrained-content class of **talking heads**
(video conferencing, newscaster/lecture capture, virtual-avatar puppeteering).
The pixel grid is a degenerate encoding for this content: a face is a
low-dimensional, highly-structured signal, and a generative/parametric model
can reproduce it from a tiny control vector far more cheaply than a
transform/entropy codec can reproduce the pixels.

The decode output is a **face parameter vector** → fed to a **synthesizer**
(the decoder's "inverse") → produces the output frame. There is no DCT, no
block partition, no in-loop filter — those are replaced by a
model-conditioned render.

### Why not just use AV1/H.264 with a lower QP?

General codecs treat the face as arbitrary natural image content. They spend
bits on background, lighting falloff, and skin micro-texture that a parametric
face model reproduces for free from ~50–300 scalars. For conferencing the
background is often static or synthetic anyway, so the structural prior the
face model carries is exactly the signal general codecs discard.

---

## Design-phase checklist

Resolved / to resolve before scaffolding:

- [x] **DECISION 1 — v1 face representation** (3DMM / sparse keypoints / learned latent). **Resolved: parametric 3DMM-style head model as primary, with sparse landmarks as a low-bitrate companion; learned latent deferred to v2.** See below.
- [x] **DECISION 2 — v1 synthesizer** (deterministic rasterizer vs. tiny neural renderer). **Resolved: deterministic 3DMM rasterizer as the mandatory v1 path; an optional, versioned neural-texture refinement is a v2 layered enhancement.** See below.
- [x] **DECISION 3 — Bitstream layout** (parameter-vector framing, keyframe vs. inter "expression delta", versioning). **Resolved: byte-aligned headers (magic/version/basis-pin/flags) + one-time identity setup + per-frame quantized parameter vectors with inter-expression/pose deltas; rANS-coded, basis hash-pinned.** See below.
- [x] **DECISION 4 — Encoder front-end** (fit source frame → parameter vector). **Resolved: v1 = landmark-driven fit (NN-free, bounded); v2 = image→coeff regression net + optional optimization refinement. Encoder method is invisible to the decoder (bitstream-agnostic).** See below.
- [x] **DECISION 5 — Loss/quality metric for v1** ("face fidelity per bit": landmark accuracy, reenactment L1/LPIPS, identity retention, or perceptual). **Resolved: primary = LPIPS reconstruction + ArcFace identity similarity (gated `face-bench`); cheap deterministic CI gate = landmark NME regression.** See below.
- [x] **DECISION 6 — Memory/perf budget** (target platform: conferencing endpoint vs. edge server; model-weight footprint ceiling). **Resolved: conferencing-endpoint envelope (RPi 5 / laptop class), same as `lean`/`vision`; decoder NN-free (0 weights) for v1; ~20 MB arena @1080p; <10 ms/frame. v2 neural-texture layer capped ~5 MB.** See below.
- [x] **DECISION 7 — Relationship to `tpt-kinetix-vision` and `tpt-kinetix-lean`** (shared primitives? shared parameter-vector transport?). **Resolved: v1 = standalone sibling crate, copy rANS/BitReader from `lean` (mirrors vision's DECISION 8); deferred extraction into `tpt-kinetix-bitstream`. No cross-dependency; reuses `core::VideoFrame` + `DecoderCapabilities`.** See below.
- [x] **DECISION 8 — Strict mode / honesty contract** (`capabilities().pixel_exact` analog; what the decoder returns when the synthesizer/model is absent). **Resolved: face is "synthesized, never pixel-exact" by design; `DecoderCapabilities` gains a `synthesized` flag + `neural_texture_present` capability; strict mode returns `NotPixelExact` when the required synthesizer/model is missing.** See below.

---

## DECISION 1: v1 face representation

The representation is *what the bitstream carries* — the control vector that
drives synthesis. Three candidate parametrizations:

### Alternative A — 3D Morphable Model (3DMM) coefficients

A 3DMM expresses a face as a sum of PCA basis shapes and appearances:

- `shape` = `mean + Σ αᵢ·eigvecᵢ` (identity) + expression basis (e.g. FLAME / FaceWarehouse)
- `appearance` / albedo = low-dim PCA of texture
- `pose` = 3D rotation + translation
- `illumination` = spherical-harmonic (SH) coefficients (optional)

A fully-specified talking head is then a compact vector: ~80 identity coeffs,
~50 expression coeffs, ~3–6 pose params, ~27 SH lighting coeffs, ~<100 albedo
coeffs. On the order of **100–300 floats** per frame — already <1 KB at f32,
and far smaller after quantization + entropy coding.

| Pros | Cons |
|---|---|
| Compact, *self-contained decode* — the decoder renders from the vector + a fixed 3DMM asset; no source frame needed. | Decode side needs a 3DMM mesh + rasterizer (or neural renderer). Heavier than keypoints but bounded and deterministic. |
| Clean separation of identity (constant across a call) from expression (per-frame delta) → excellent inter-frame coding. | Fitting a 3DMM from a 2D source frame requires a regression/optimization front-end (encoder complexity, not decoder). |
| Strong prior: extrapolates well within the face manifold; stable, interpretable params. | Can't reproduce off-manifold content (hands over face, extreme poses) — acceptable for conferencing. |
| Maps naturally to a hierarchical bitstream (identity sent once, per-frame expression deltas). | Assets (basis vectors) must ship with the codec and be versioned. |

### Alternative B — Sparse facial landmarks

A fixed set of 2D (or 3D) points — e.g. 68-point IBUG, MediaPipe FaceMesh 468,
or a conferencing-tuned subset. Hundreds of floats encoding *where* facial
features are.

| Pros | Cons |
|---|---|
| Trivially compact and easy to encode/decode; huge existing ecosystem (dlib, MediaPipe). | **Not a self-sufficient synthesis target.** Landmarks alone cannot paint photoreal pixels — they are a control/wireframe signal, not a renderable representation. |
| Deterministic, no model weights beyond a detector. | Requires a *separate generative model* (GAN / diffusion / neural renderer) conditioned on landmarks to produce output. That generator is the real decoder, and it is a learned model (see C). |
| Excellent as an ultra-low-bitrate mode or avatar-drive signal. | No appearance/identity/lighting info — the synthesizer must invent or carry those separately. |

Landmarks are best understood as a **companion/fallback** representation, not
the primary v1 decode target: ship them when the link is too constrained for a
full parameter vector, or use them to drive a non-photoreal avatar overlay.

### Alternative C — Learned latent code

A learned embedding (`face-vid2vid` / StyleGAN / autoencoder latent, or a
face-reenactment code) conditioned by a driving signal. Very compact and
high visual quality.

| Pros | Cons |
|---|---|
| Highest visual quality at the lowest bit cost when the model fits. | The decoder **is a neural network** whose weights must ship with the codec and be fixed/versioned. Contradicts this project's memory-bounded, embedded-friendly, deterministic ethos for v1. |
| Generalizes within the talking-head domain. | Fragile outside the training manifold; quality is model-version-coupled; hard to guarantee bounded decode time/memory. |
| | Conflicts with the "original, auditable, deterministic bitstream" identity of the `lean`/`vision` sibling codecs. |

Defer to **v2** once a sufficiently small, fixed, versioned renderer exists
(e.g. a tiny MLP neural renderer or a distilled face-vid2vid-lite). v1 should
not require shipping an NN decoder.

---

### Recommendation

**Primary v1 representation = a parametric 3DMM-style head model** (shape +
expression + appearance + pose + optional SH illumination), carried as a
quantized/entropy-coded coefficient vector. Concretely:

1. **Canonical representation:** 3DMM coefficient vector.
   - Identity basis sent **once per call** (keyframe); per-frame payload is the
     **expression + pose delta** (tiny, highly compressible). This is the
     inter-frame coding win that makes the codec competitive.
   - Appearance/albedo sent at low rate (faces change little within a call);
     SH illumination updated slowly or held constant.
2. **Companion / fallback:** a **sparse-landmark vector** as a lowest-bitrate
   mode (and as the natural bridge to avatar/AR overlays). It rides the same
   bitstream framing but triggers a lighter synthesizer path.
3. **Deferred:** **learned latent code** to v2, behind a fixed, versioned,
   size-bounded renderer — not a v1 blocker.

**Why this ordering fits the project:** it keeps v1's decoder *deterministic
and asset-bounded* (a 3DMM mesh + rasterizer or a small fixed renderer, no
shipping of a general NN), it yields a genuinely compact bitstream (the whole
point of the codec), and it leaves a clean upgrade path (swap the synthesizer
for a neural renderer in v2 without changing the parameter-vector transport).

**Synthesizer implication (DECISION 2, next):** for v1 the renderer should be
**deterministic** — a 3DMM mesh rasterization (possibly with a small, fixed
"neural texture / shading" refinement) rather than a full generative model —
so decode cost and memory stay bounded and auditable, consistent with
`tpt-kinetix-lean`'s embedded envelope.

---

## DECISION 2: v1 synthesizer

The synthesizer consumes the parameter vector (shape + expression + appearance +
pose + illumination) and produces the output frame. The representation
(DECISION 1) is *what is carried*; this decision is *how it is turned back into
pixels on the decode side*.

### Alternative A — Deterministic 3DMM rasterizer

Classical computer-graphics render: mesh from the shape basis → albedo texture
atlas (appearance coeffs) → spherical-harmonic Lambertian shading (illumination
coeffs) → rasterize with z-buffer + bilinear texture sampling → optional fixed
tone/gamma post. No neural network anywhere in the path.

| Pros | Cons |
|---|---|
| Fully deterministic, auditable, reproducible — fits the project's memory-safe / bounded / embedded identity (`tpt-kinetix-lean` envelope). | Output looks "CG", not photoreal: hair, teeth, eyes, and fine skin detail are weak; Lambertian SH can't capture specular/subsurface. |
| No model weights to ship or version; decoder is just geometry + rasterizer. | Needs a texture atlas in the appearance payload (or a fixed mean albedo). |
| Tiny, bounded memory (mesh + atlas) and trivial decode time (rasterize a few-thousand-tri mesh). | Sensitive to 3DMM fit error on the encode side — a bad pose/shape fit shows as a clearly wrong (not just blurry) face. |
| Naturally aligns with the honesty/dual-path contract (see DECISION 8). | |

### Alternative B — Neural-texture renderer (small, fixed)

Keep the 3DMM geometry deterministic; rasterize to a *feature* buffer, then run
a small fixed conv net ("neural texture" / deferred neural rendering, à la
RGB→feature atlas + lightweight U-Net) to produce photoreal RGB. Only the final
shading/texture stage is learned.

| Pros | Cons |
|---|---|
| Photoreal quality at modest extra cost; geometry stays deterministic and auditable. | Ships NN weights (size-bounded but versioned); model-version coupling. |
| Small net (a few conv layers, ~1–5 MB) — still bounded, still far cheaper than a full generative model. | Decode time/memory now has a NN term; determinism holds only if the net is fixed per version. |
| Standard, well-understood approach (Neural Texture / RIS / face-vid2vid-lite). | A decoder without the model weights can't produce this output — needs a capability flag. |

### Alternative C — Full generative model (face-vid2vid / StyleGAN-driven)

Essentially the learned-latent path from DECISION 1 (Alt C). Highest quality
but the decoder *is* a versioned generative network — conflicts with v1's
deterministic/embedded goal. **Deferred to v2** (and only if a small enough
fixed model exists).

### Recommendation

**Mandatory v1 synthesizer = Alternative A (deterministic 3DMM rasterizer).**
The v1 decoder must be auditable, deterministic, and asset-bounded, with no
neural network to ship or version — consistency with `tpt-kinetix-lean`'s
embedded envelope and the project's honesty contract is the priority over
photorealism for a first release.

**Layered enhancement = Alternative B (neural-texture refinement) as a v2
optional path**, gated behind a bitstream flag so a decoder *without* the model
still produces valid (if less photoreal) output. This is the same dual-path
shape as `tpt-kinetix-vision` (tensor vs. pixels): the deterministic rasterizer
is the guaranteed path; the neural refinement is an opt-in quality layer. DECISION 8
(honesty contract) must expose whether the refinement model is present.

Concretely, v1 decode is:

1. Reconstruct mesh vertices + normals from shape/expression coeffs (matrix–vector, fixed basis).
2. Place mesh via pose coeffs; shade via SH illumination coeffs + albedo atlas (appearance coeffs).
3. Rasterize (z-buffer, bilinear sampling) → RGB frame.
4. *v2 only:* if the stream declares a neural-texture layer and the decoder has the matching fixed model, refine the rasterized buffer → photoreal output.

**Asset implications (feeds DECISION 6):** the decoder needs the 3DMM basis
vectors (shape/expression/mean albedo) and the SH basis shipped as a fixed,
versioned asset. Size is small (basis matrices are KB–low-MB), but the asset
version must be pinned in the bitstream so old decoders reject/upgrade
gracefully.

---

## DECISION 3: bitstream layout

Carries the parameter vector (DECISION 1) to the synthesizer (DECISION 2). The
load-bearing idea: **identity is sent once per call; per-frame payloads are
small expression + pose deltas** — that delta structure is what makes the codec
compress talking heads at all.

### Sequence header

Byte-aligned (spend framing budget on the payload, not headers — same rationale
as `tpt-kinetix-lean` / `tpt-kinetix-vision`).

| Field | Type | Notes |
|---|---|---|
| `magic` | `[u8; 4]` | `b"FACE"` |
| `version` | `u8` | Format version (`1` for v1) |
| `asset_basis_id` | `u8` | Index into built-in 3DMM basis set (shape/expression/mean-albedo/SH) — see DECISION 2 asset note |
| `basis_hash` | `[u8; 8]` | Truncated hash of the exact basis asset, so a decoder with a *mismatched* basis rejects rather than silently rendering a wrong face |
| `max_width` | `u16` BE | Max frame width (arena sizing) |
| `max_height` | `u16` BE | Max frame height |
| `flags` | `u8` | bit0 = `landmark_companion`, bit1 = `neural_texture_layer` (v2 opt-in; see DECISION 2/8) |
| `quant_precision` | `u8` | Fractional bits for coefficient quant steps (0 = integer) |
| `group_qp` | `[u8; 5]` | Per-group base quant steps: `[identity, expression, pose, illumination, appearance]` (frames may override per-group) |

### Call / identity setup

One block per session (a "call" = one talking head, identity constant):

| Field | Type | Notes |
|---|---|---|
| `identity_coeffs` | quantized vector | Shape/identity basis weights; sent **once**, reused by every subsequent frame |
| `albedo_override` | optional vector | Override of mean albedo if the stream carries a per-speaker texture (else decoder uses basis mean albedo) |

### Frame header

| Field | Type | Notes |
|---|---|---|
| `frame_type` | `u8` | Key (0) = full vector incl. identity; Inter (1) = deltas only |
| `width` | `u16` BE | ≤ `max_width` |
| `height` | `u16` BE | ≤ `max_height` |
| `ref_mode` | `u8` | Delta reference: 0 = previous frame, 1 = last key (for error resilience / scene cut) |
| `group_qp_override` | optional `[u8; 5]` | Per-frame quant tweak |
| `payload_len` | `u32` BE | Length of the rANS-coded parameter payload |

### Parameter payload (rANS-coded)

| Group | Key frame | Inter frame |
|---|---|---|
| Identity | full vector (redundant w/ setup, but self-contained key) | *omitted* (from setup) |
| Expression | full vector | **delta** from reference frame |
| Pose | full vector | **delta** from reference frame |
| Illumination (SH) | full vector | small delta (often zero) |
| Appearance / albedo | full or override | small delta (often zero) |

Each group is its own rANS sub-stream (reuse `RansStreamSet` from
`tpt-kinetix-lean`, per DECISION 7). Deltas are zero-centered and correlated →
a Laplacian / zero-biased symbol model compresses them far better than the
full-vector key path.

### Landmark companion block (optional, `flags.bit0`)

When enabled, each frame may also carry a quantized **sparse-landmark vector**
(DECISION 1 companion mode) as a separate rANS sub-stream. A rasterizer decoder
can use it to *drive the mesh* (landmark → mesh fit); a pure-avatar/overlay
decoder can consume it directly without any 3DMM basis. It never prevents a
basis decoder from working — it is additive framing.

### Bitstream structure (summary)

```
[Face Sequence Header]
[Call / Identity Setup]        (identity sent once per session)
[Frame Header]
[RANS: expression delta]
[RANS: pose delta]
[RANS: illumination delta]
[RANS: appearance delta]
[Optional RANS: landmark companion]
```

### Recommendation

Adopt the layout above, unchanged in shape from the sibling codecs:
byte-aligned headers, `max_*` arena sizing, magic + version, and a
**basis-hash pin** so asset/version drift fails loudly instead of producing a
silently-wrong face. Inter-frame coding is **expression+pose deltas** (identity
constant), which is the core compression win for the talking-head class. The
`neural_texture_layer` flag is declared here but only exercised by DECISION 2's
v2 path; a v1 decoder without that model must still decode the rasterizer path
(DECISION 8).

---

## DECISION 4: encoder front-end

How a source frame becomes the parameter vector (DECISION 1) that the bitstream
(DECISION 3) carries to the synthesizer (DECISION 2). Crucial property: **the
encoder method is invisible to the decoder** — the bitstream only carries the
resulting coefficients, so the format stays clean/auditable regardless of how
the fit was produced.

### Alternative A — Image→coefficient regression network

A feed-forward net (DECA / 3DDFA-v2 / MGCNet-style encoder) maps a cropped face
image straight to 3DMM coeffs in one pass.

| Pros | Cons |
|---|---|
| Real-time (ms/frame), parallelizable, deterministic given fixed weights. | Ships NN weights on the *encoder* (model-version coupling, trained-asset dependency). |
| Handles unconstrained input (no explicit landmarks needed). | Needs a Rust inference runtime (`burn`/`tract`) to stay self-contained — real dependency/complexity for v1. |
| Best quality for live conferencing. | Quality is training-data-bound; failure modes (no face / occlusion) need handling. |

### Alternative B — Optimization / analysis-by-synthesis fit

Iteratively render current params and minimize image-vs-render error (classical
3DMM fitting, differentiable renderer).

| Pros | Cons |
|---|---|
| No training data; high-quality fit; fully classical. | Slow (tens–hundreds of iters); **timing not bounded/deterministic**. |
| | Unsuitable for real-time conferencing encode; CPU-heavy. |

### Alternative C — Landmark-driven fit (shape-from-landmarks)

Detect/consume 2D landmarks, fit the 3DMM from them: **identity/shape once per
call, expression + pose per frame** (the sparse-landmark companion block from
DECISION 1/3 *is* this signal). Aligns with the crate's "landmark-driven
synthesis" charter.

| Pros | Cons |
|---|---|
| **NN-free and bounded** for v1 — preserves the project's deterministic/embedded identity on *both* encoder and decoder. | Needs a landmark source: bundled lightweight detector, or landmarks supplied as input. |
| Reuses the companion-landmark block as both encoder drive signal and bitstream fallback. | Classical landmark detectors (CLM/ESR) are fiddly; pure landmark fit misses subtle expression nuance a full image net captures. |
| Directly produces the expression/pose deltas the bitstream already carries. | |

### Recommendation

**v1 encoder = Alternative C (landmark-driven fit).** It keeps v1 fully
deterministic and NN-free on both sides of the codec, directly yields the
expression/pose deltas the bitstream already carries, and reuses the
sparse-landmark companion block as the driving signal. The encoder accepts
either pre-computed landmarks (from an external detector / the companion block)
or runs a bundled lightweight classical detector; resolving the detector
dependency is the one v1 sub-question (see open questions).

**v2 encoder = Alternative A** (image→coeff regression net, single forward pass
via a fixed Rust inference runtime) for real-time, higher-quality,
landmark-free encoding, **optionally + a few Alternative B refinement steps**
(DECA-style hybrid) for offline/batch. The decoder is unchanged — it only ever
sees coefficients.

This sequencing means v1 ships a complete, auditable, NN-free codec (encoder +
decoder) true to the project identity, with a clean upgrade path to NN
encoders later without touching the bitstream or the deterministic decoder.

---

## DECISION 5: loss / quality metric for v1

A face codec has no meaning without an objective function: every quantization
choice (DECISION 3 group QPs) and basis choice (DECISION 2) must be measured
against something. The metric must capture the two real failure modes of a
*parametric* face codec: (1) the expression/pose is wrong, and (2) **the person
drifts into looking like someone else** (identity loss) — which pixel codecs
don't suffer but a 3DMM fit absolutely can.

### Alternative A — Landmark accuracy (NME)

Normalized mean error between landmarks of the synthesized face and the source.
Fast, deterministic, **model-free**.

| Pros | Cons |
|---|---|
| Cheap; runs in normal CI as a regression gate (no weights). | Only measures geometry, not appearance/identity/photorealism. |
| Directly tied to the v1 landmark-driven control signal. | Insensitive to albedo/lighting/identity drift. |

### Alternative B — Reconstruction fidelity (L1 / SSIM / LPIPS)

Compare synthesized frame to source frame. LPIPS is perceptually aligned; SSIM/PSNR
penalize the same human-oriented artifacts general codecs do (less face-relevant).

| Pros | Cons |
|---|---|
| Captures appearance + expression + identity together in one number. | SSIM/PSNR are the wrong objective for a face task (same trap as general codecs). |
| LPIPS is a decent perceptual proxy. | Needs a perception model (weights) — gated, not a plain CI gate. |

### Alternative C — Identity retention (ArcFace cosine similarity)

Face-recognition embedding similarity between synthesized and source. Measures
"still the same person" — the paramount conferencing property.

| Pros | Cons |
|---|---|
| Directly guards the identity-drift failure mode. | Needs a recognition model (weights); not a plain CI gate. |
| Compact single scalar; robust to background/lighting. | Doesn't by itself catch expression mismatch. |

### Alternative D — Downstream task suite

Run face recognition / emotion / lip-reading on synthesized vs. source; compare
task accuracy. Most "task-faithful" (mirrors `tpt-kinetix-vision`'s mAP-vs-bitrate
philosophy) but heaviest to run.

### Recommendation

A **two-tier** metric, matching `tpt-kinetix-vision`'s primary-task + cheap-gate split:

1. **Cheap deterministic CI gate = landmark NME (Alt A).** Model-free, runs in
   every CI pass as a regression guardrail. Because landmarks are the v1 control
   signal, a rising NME immediately flags a bad quant/encoder change.
2. **Primary v1 objective = LPIPS reconstruction + ArcFace identity similarity
   (Alt B + C).** Together they cover the appearance/expression *and* the
   identity-drift failure modes. Gated behind a `face-bench` feature flag
   (downloads LPIPS + ArcFace weights), exactly like vision's `vision-bench`.

The benchmark harness (the thing DECISION 5 exists to define):
1. Encode a reference talking-head clip at 5+ `group_qp` levels → 5+ bitrate points.
2. Decode each with the rasterizer (DECISION 2).
3. Measure LPIPS + ArcFace vs. source, and landmark NME as the fast guard.
4. Plot **"face fidelity vs. bitrate"** (LPIPS↓/ArcFace↑ vs. bits) and save as a test artifact.

This curve is the v1 acceptance gate and the tool for tuning the DECISION 3
quant steps and DECISION 2 basis. Defer the full downstream task suite (Alt D)
to a later phase — it is the natural v2 evolution, as mAP was for vision.

---

## DECISION 6: memory / perf budget

The v1 decoder is a deterministic rasterizer over a fixed 3DMM asset
(DECISION 2) — **no neural network on the decode path**. That makes the budget
dominated by geometry + framebuffers, not weights, which is exactly the
embedded-friendly envelope this project favors.

### Constraints (v1)

| Constraint | Value | Rationale |
|---|---|---|
| Max resolution | 1920×1080 | Matches `lean`/`vision` v1; covers all conferencing endpoints |
| Decode arena ceiling | ~20 MB @1080p | Framebuffer + z-buffer + normal/feature buffers (`1920*1080 * ≤3`) + mesh + basis workspace |
| 3DMM basis asset | < 2 MB (ROM-like, loaded once) | Shape/expression/mean-albedo/SH PCA matrices; pinned by `basis_hash` (DECISION 3) |
| Albedo texture atlas | 256–512² RGB (~0.2–0.75 MB) | Carried in appearance payload or basis mean |
| **Decoder NN weights** | **0** | v1 synthesizer is rasterization only (DECISION 2) |
| Target decode time | < 10 ms/frame @1080p | Rasterizing a few-thousand-tri mesh + texture fill is bounded; matches `lean`/`vision` embedded target |
| `no_std` / MCU | Future work, not v1 | Same deferral as `lean`/`vision` — prove the alloc-free hot path on embedded Linux first |

### Target platform

| Alternative | Tradeoff |
|---|---|
| **A) Conferencing endpoint (RPi 5 / laptop class)** | Maximizes overlap with `lean`/`vision`; the decode is cheap enough to sit comfortably in their envelope. The real deployment target (in-call endpoints, edge conferencing boxes). |
| **B) Edge-inference server (Jetson / x86 edge)** | Higher budget enables the v2 neural-texture layer now; but v1 doesn't need it, and conferencing endpoints are the tighter, more valuable constraint. |

**Recommendation: (A) conferencing-endpoint envelope**, identical to
`lean`/`vision` v1. v1 needs no server-class budget because the decoder ships
zero model weights.

### Model-weight footprint ceiling

This is the budget that matters for **v2**, not v1:

- The **v2 neural-texture refinement** (DECISION 2, Alternative B) must be
  capped at **~1–5 MB** of fixed, versioned weights, declared via the
  `neural_texture_layer` flag (DECISION 3) and exposed through the honesty
  contract (DECISION 8). A decoder without the model still decodes the
  rasterizer path, so the weight ceiling is a quality ceiling, not a hard
  decode requirement.
- The **v2 regression-net encoder** (DECISION 4) ships weights too, but that
  runs in controlled encode infra, not on the constrained endpoint, so it is
  out of this budget.

---

## DECISION 7: relationship to `vision` and `lean`

All three are *original* bitstream codecs in this workspace. Face should reuse
what already exists rather than reinvent it, but without taking a dependency
direction that breaks the publish-order invariant (codecs depend only on
`core`).

### Alternative A — Standalone, copy primitives

Face is a sibling crate; copy `BitReader` + rANS (`RansEncoder`/`RansDecoder`/
`RansStreamSet`) from `lean` initially, as `vision` did (vision DECISION 8 =
"start independent, extract later").

| Pros | Cons |
|---|---|
| No shared-dependency coordination; face depends only on `core`. | Two (soon three) copies of rANS to maintain until extracted. |
| Mirrors the established `vision` precedent — consistent contributor story. | |

### Alternative B — Shared `tpt-kinetix-bitstream` crate

Extract `lean`'s bitreader + rANS into a workspace crate; `lean`/`vision`/`face`
all depend on it.

| Pros | Cons |
|---|---|
| Single source of truth for entropy/bitstream primitives. | Requires refactoring `lean` now; `vision` already deferred this exact extraction. Premature until all three codecs are stable. |

### Alternative C — Face depends on `lean` (or `vision`)

Reuse `lean`'s rANS directly via a crate dependency.

| Pros | Cons |
|---|---|
| Zero duplication immediately. | Breaks the "codecs depend only on `core`" publish-order invariant; a face codec depending on a generic video codec is a confusing dependency direction. |

### Alternative D — Shared parameter-vector transport with `vision`

Unify face's coeffs and vision's `Tensor` under one "parametric output" type.

| Pros | Cons |
|---|---|
| Tidy abstraction. | Different output shapes (coeffs vs. feature map); forces a premature union. Not warranted for v1. |

### Recommendation

**v1 = Alternative A** (standalone sibling crate, copy primitives from `lean`),
exactly as `vision` scoped its DECISION 8. Do **not** take a dependency on
`lean`/`vision` (keeps the publish-order invariant: codecs → `core` only).

**Deferred = Alternative B.** Once `lean`, `vision`, and `face` are all stable,
extract `tpt-kinetix-bitstream` (BitReader + rANS + `SymbolModel`) and have all
three depend on it — the extraction both sibling codecs already planned for.

Face-specific reuse from `core`:
- **`VideoFrame`** — the synthesizer (DECISION 2) produces a `VideoFrame` for
  human review, so face reuses the existing core frame type rather than
  defining its own.
- **`DecoderCapabilities`** — face follows the same honesty contract as the
  other codecs (`capabilities().pixel_exact` analog; DECISION 8). The face
  decoder is *not* pixel-exact vs. the source frame by design (it synthesizes),
  so its capability flag must express "synthesized, not reconstructed" rather
  than claim pixel-exactness.

**Worth noting (not a v1 decision):** a face's parameter vector is itself a
compact downstream representation for face-analysis ML (recognition / reenactment
/ emotion). Face could later expose a `vision`-style dual path — `decode_params()`
(fast, for ML) vs. `decode_pixels()` (synthesized frame, for review) — but that
is a v2 evolution, not part of the v1 bitstream.

---

## DECISION 8: strict mode / honesty contract

This codec's output is **synthesized**, not reconstructed — by design it is
never bit-exact vs. the source frame (it redistributes bits into a parametric
model). The project's honesty contract (every other codec exposes
`DecoderCapabilities`, and strict mode returns `KinetixError::NotPixelExact`
rather than silently emitting wrong pixels) must apply here too, but the
semantics differ: "not pixel-exact" is *expected*, not a defect.

### What the contract must express

| State | Meaning |
|---|---|
| `synthesized = true` | Output is a model synthesis of the face, not a pixel reconstruction. Normal and intended. |
| `pixel_exact = false` | Always false for face — there is no pixel-exact path by design. |
| `neural_texture_present` | Whether this decoder has the v2 neural-texture model (DECISION 2) to refine the rasterizer output. |
| `basis_available` | Whether the pinned 3DMM basis (`basis_hash`, DECISION 3) is present and matches. |

### Behaviour when a required asset is missing

| Case | Non-strict | Strict |
|---|---|---|
| Basis asset missing / hash mismatch | Return `Ok(None)` (no output) | Return `KinetixError::NotPixelExact` (or a new `AssetMissing` error) |
| `neural_texture_layer` flagged but model absent | Fall back to rasterizer output (still valid, less photoreal) | Fall back to rasterizer output **and** report `neural_texture_present = false`; only error if the stream *requires* the layer (future flag) |
| Truncated / corrupt parameter payload | Fail safe per rANS decode error | Same |

### Recommendation

1. **Extend `DecoderCapabilities`** (in `tpt-kinetix-core`) with a
   `synthesized: bool` field (and keep `pixel_exact` always `false` for face) so
   callers/CLI can distinguish "synthesized face" from "incomplete pixel
   decoder" — both currently surface via `NotPixelExact`, which would be
   misleading for an *intended* synthesis.
2. **Add `neural_texture_present` + `basis_available`** to the face decoder's
   capability report, so a caller knows whether it is getting rasterizer-only or
   refined output, and whether the pinned basis loaded.
3. **Strict mode semantics:** like the other codecs, strict mode returns an
   error instead of silently emitting placeholder/wrong output — specifically
   `KinetixError::NotPixelExact` when the pinned basis is absent, so a
   conformance/CI run fails loudly rather than rendering a default-mean face and
   pretending it succeeded.
4. **A missing optional neural-texture model is NOT an error** (graceful
   fallback to the rasterizer), but it is reported via capability so the choice
   is observable.

This keeps face consistent with the workspace's "never return wrong data
silently" rule while correctly communicating that synthesis — not
reconstruction — is the whole point.

---

## Open questions carried into later decisions

1. **Which 3DMM basis?** FLAME (expression + identity + jaw/neck) vs.
   FaceWarehouse vs. a project-trained basis. Asset licensing/size and
   decoder footprint drive this (DECISION 2/6).
2. **Identity-vs-expression split point.** How many identity coeffs to send
   once vs. carry per-frame. Tunable; affects keyframe size.
3. **Background handling.** Talking-head frames have a background — is it a
   static crop+composite (encoder-side), a separate low-rate layer, or out of
   scope (assume synthetic/blurred bg)? Relevant to DECISION 3/7.
4. **Landmark companion granularity.** Which landmark set (68 vs. a
   conferencing subset) for the fallback mode, and how it maps onto the
   primary synthesizer (drives mesh, or drives a separate avatar path)?

5. **Encoder landmark source (DECISION 4).** Bundle a lightweight classical
   detector (CLM/ESR) or require pre-computed landmarks as input for the v1
   landmark-driven fit?

---

## Implementation order (post-design resolution)

The crate is scaffolded (`tpt-kinetix-face/`) with `FaceDecoder`,
`FaceParams`, and the `FaceSynthesizer` trait seam, reporting
`pixel_exact = false` honestly. Resolve the open questions above as each step
lands. Entropy/rANS primitives should reuse `tpt-kinetix-bitstream` (now
extracted in the workspace) rather than copying them — the DECISION 7
"deferred extraction" has already happened.

1. **Basis asset + loader.** Pick the 3DMM basis (open question 1); ship shape/expression/mean-albedo/SH PCA matrices as a fixed, versioned asset; load + verify via `basis_hash` (DECISION 3). **DONE** — `src/basis.rs`: a fixed, versioned `BasisAsset` (base mesh + identity/expression displacement bases + mean albedo) with an FNV `basis_hash`; `load_basis`/`load_from_header` verify the stream's pinned hash and reject mismatches (DECISION 8). The built-in basis is a **deterministic placeholder** (procedural head proxy); selecting a production 3DMM (FLAME / FaceWarehouse) remains open question 1 — the loader is asset-agnostic so it is a data change, not a code change.
2. **Sequence / frame header parsing** (byte-aligned, `magic`/`version`/`flags`/`group_qp`). Fail loudly on version/hash mismatch (DECISION 3, 8). **DONE** — `src/header.rs`: `FaceSequenceHeader` / `FaceFrameHeader` with nom parsers, magic/version/truncation validation, and (de)serialization round-trip tests. The `basis_hash` match is delegated to the caller (the basis loader from step 1).
3. **Parameter-vector decode:** rANS-decode the 5 coefficient groups; identity once per call, per-frame expression/pose deltas; per-group dequantization from `group_qp` (DECISION 3). Reuse `tpt-kinetix-bitstream`'s `RansStreamSet` / `SymbolModel`. **DONE** — `src/params.rs`: `FaceParamCodec` rANS-codes the five groups as independent `RansStreamSet` sub-streams (key = all five; inter = the last four deltas), with a zero-biased `FaceCoefModel` (`SymbolModel`) that concentrates mass on small magnitudes; per-group `group_qp` (or per-frame override) dequantization with `quant_precision` fractional bits. Round-trip + error-path tests pass.
4. **`FaceSynthesizer` impl — deterministic rasterizer** (DECISION 2): reconstruct mesh from shape/expression coeffs, place via pose, SH-shade albedo atlas, rasterize → `VideoFrame`. This flips the decoder from `Ok(None)` to real synthesis.
5. **Honesty contract wiring** (DECISION 8): extend `DecoderCapabilities` with `synthesized` (and `neural_texture_present` / `basis_available`); strict mode returns `NotPixelExact` when the pinned basis is absent.
6. **Landmark companion path** (DECISION 1): parse the optional sparse-landmark rANS block; mesh-from-landmarks drive + avatar fallback.
7. **Encoder front-end** (DECISION 4): landmark-driven fit (v1, NN-free) producing identity-once + per-frame deltas; the `face-bench` loop closes the encode/decode round trip.
8. **Benchmark harness + metric** (DECISION 5): encode a reference clip at 5+ `group_qp` → decode → LPIPS + ArcFace + landmark NME → plot "face fidelity vs. bitrate"; tune `group_qp` + basis. Landmark NME as a plain CI regression gate.
9. **Fuzz target** for the header + parameter-vector parser (parity with the other codec fuzz targets; excluded from the workspace build, nightly `cargo-fuzz`).
10. **v2 (deferred):** neural-texture refinement layer (≤5 MB, versioned, optional) behind `neural_texture_layer`; image→coeff regression-net encoder (DECISION 2/4).
