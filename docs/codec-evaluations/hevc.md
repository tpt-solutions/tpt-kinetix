# Codec Evaluation: HEVC / H.265 Video Decode

**Status**: Under evaluation — High-priority long-term candidate  
**Last updated**: Phase 9 evaluation

---

## Overview

HEVC (High Efficiency Video Coding), standardised as H.265 and ITU-T H.265 / ISO
14496-10, is the dominant codec for 4K streaming (Netflix, Apple TV+, Disney+, YouTube
4K) and Blu-ray Ultra HD. It achieves roughly 2× the compression efficiency of H.264
at equivalent visual quality. Any media pipeline that targets modern 4K content must
eventually support HEVC decode.

---

## Technical Complexity

**Complexity rating: Very High**

HEVC is substantially more complex than H.264 and represents the largest implementation
challenge in the Kinetix roadmap. Key sources of complexity:

### CTU/CU/PU/TU Hierarchy

HEVC replaces H.264's fixed 16×16 macroblock with a recursive quad-tree structure:

- **CTU (Coding Tree Unit)**: the top-level coding unit, 64×64 pixels in most profiles.
- **CU (Coding Unit)**: a CTU is recursively split into CUs down to a minimum of 8×8.
  Each CU is either intra- or inter-coded.
- **PU (Prediction Unit)**: controls the motion compensation partition shape within a CU.
  HEVC supports 8 partition types per CU (2Nx2N, 2NxN, Nx2N, NxN, and four asymmetric
  partitions).
- **TU (Transform Unit)**: the residual is coded in TUs of 4×4, 8×8, 16×16, or 32×32,
  each recursively split from the CU.

Correctly decoding this quad-tree requires careful recursive state tracking that is
more complex to parallelise than H.264's macroblock rows.

### CABAC (Context-Adaptive Binary Arithmetic Coding)

HEVC uses CABAC exclusively — there is no CAVLC fallback. The HEVC CABAC context model
has approximately 154 context variables (compared to H.264's 460, but HEVC's contexts
are more complex). Context initialisation, renormalisation, and bypass mode must all
be implemented precisely. Any off-by-one error in the arithmetic coder produces
cascading decode failures.

### Intra Prediction Modes

HEVC defines 35 intra prediction modes (DC, planar, and 33 angular modes), compared
to H.264's 9. The angular prediction interpolation requires a reference sample
smoothing filter and a position-dependent phase calculation. All 35 modes must be
implemented and validated.

### SAO (Sample Adaptive Offset) Filtering

SAO is an in-loop filter applied after deblocking that corrects for quantisation
artefacts using per-CTU offset parameters. It adds a second in-loop filter pass that
does not exist in H.264.

### Additional Complexity Sources

- **Dependent slice segments**: allow slices to share entropy coder state, complicating
  parallel decode.
- **WPP (Wavefront Parallel Processing)**: HEVC defines an entropy-level parallelism
  scheme (WPP); implementing it correctly requires careful context re-initialisation
  at the start of each CTU row.
- **Tiles**: a grid partition that enables independent parallel decode regions; simpler
  to parallelise than WPP but requires tile boundary handling in the loop filters.
- **Scaling lists**: large per-transform-size quantiser matrices that must be parsed
  and stored correctly.
- **RPL (Reference Picture List) management**: more complex than H.264's frame reordering
  due to long-term reference pictures and the RPS (Reference Picture Set) syntax.

---

## KG Tool Applicability

**KG applicability rating: Medium**

FFmpeg's HEVC implementation spans multiple files:

| File | Lines (approx.) | Role |
|------|----------------|------|
| `hevcdec.c` | ~3 800 | Main decoder loop, slice/CTU dispatch |
| `hevc_cabac.c` | ~2 500 | CABAC syntax element decoding |
| `hevc_filter.c` | ~1 400 | Deblocking + SAO |
| `hevc_refs.c` | ~600 | Reference picture list management |
| `hevcpred.c` | ~1 600 | Intra prediction |
| `hevc_mvs.c` | ~800 | Motion vector derivation |

Total: approximately 10 700 lines across primary decoder files; additional SIMD
optimisation files bring the total to ~20 000 lines.

The `tpt-kinetix-kg` ingest pass will handle this successfully — tree-sitter-c can parse
large files without issue. However, the resulting graph will be large (~8 000–12 000
nodes), and the dependency analysis will identify fewer clean parallelism opportunities
than for simpler codecs because HEVC's CTU quad-tree creates more inter-node
dependencies within a frame.

Expected ingestion behaviour:

- The WPP and tile parallelism points will be correctly identified as independent
  CTU rows/columns in the LoopBody analysis.
- CABAC decoding nodes will be correctly flagged as serial (no independent sets within
  a slice segment).
- The PU/TU partition dispatch will produce complex SwitchCase nodes that require
  careful hand-editing to implement correctly.

Recommendation: use `libclang` (not `tree-sitter-c`) for HEVC ingestion to get
accurate struct layout information for the `HEVCContext`, `HEVCFrame`, and `HEVCPPS`
structures. The libclang backend resolves nested typedefs that are common in FFmpeg's
HEVC implementation.

---

## Rust Ecosystem

**No mature pure-Rust HEVC decoder exists.**

Current landscape:
- `openh264` crate — wraps Cisco's OpenH264 library, covers H.264 only, not HEVC.
- `dav1d` / `rav1e` — AV1 only, not applicable.
- `hevc-rs` — an abandoned incomplete implementation; not suitable as a dependency.
- Hardware decode via `vaapi`/`videotoolbox`/`d3d11va` is available through OS APIs
  but requires platform-specific FFI and cannot be used as a pure-Rust software decoder.

This gap makes a native Kinetix HEVC implementation valuable and strategically
important. However, the lack of a reference Rust implementation also means there is
no "wrap an existing crate" shortcut as there is for AAC.

---

## Recommendation

**Prioritise HEVC after the H.264 decoder reaches pixel-exact correctness; reuse the
CABAC and entropy-decoding infrastructure developed in Phase 3.**

Rationale:

1. **Shared foundations**: HEVC and H.264 share CABAC as the entropy coding method,
   though with different context models. The CABAC engine implemented for H.264 can be
   refactored into a shared `tpt-kinetix-entropy` crate and reused, significantly reducing
   HEVC effort.

2. **CTU complexity requires stable base**: the recursive CTU/CU/PU/TU decoder is easier
   to implement and debug when the developer already has H.264's macroblock decoder as
   a reference point. Attempting HEVC without prior H.264 experience substantially
   increases risk.

3. **Market demand**: HEVC is the dominant 4K codec and is required for compatibility
   with modern streaming platforms. It should be the second video codec after H.264.

4. **AV1 learnings**: the AV1 decoder (Phase 4) and the HEVC decoder share the concept
   of superblock/CTU-level independence and tile parallelism. Implement Phase 4 before
   Phase 9 HEVC work to benefit from those learnings.

### CABAC→CABAC Note

Unlike H.264 (which also has CAVLC), HEVC is CABAC-only. Phase 3 will develop both
CAVLC and CABAC; for HEVC, only the CABAC engine is needed but the context model is
completely different (~154 HEVC contexts vs ~460 H.264 contexts). The two context
models are not compatible; plan for a separate HEVC CABAC context initialisation
table.

---

## Estimated Effort

| Stage | Effort |
|-------|--------|
| KG ingestion + graph analysis | 2–3 days |
| KG codegen + scaffold review | 1–2 days |
| CTU quad-tree parser | 3–4 weeks |
| CABAC context model | 1–2 weeks |
| Intra prediction (35 modes) | 2–3 weeks |
| Inter prediction + RPL | 2–3 weeks |
| SAO + deblocking | 1–2 weeks |
| WPP / tile parallelism | 1–2 weeks |
| Pixel-diff validation harness | 3–5 days |
| Fuzz + conformance | 1 week |
| **Total (correct implementation)** | **12–16 weeks** |
| **Total ("good enough for testing" scaffold)** | **4–6 weeks** |

The "good enough for testing" estimate covers main-profile decode of I-frames and
simple P-frames only; B-frame and 4K high-tier support requires the full estimate.

---

## Priority

**High long-term**

HEVC is the dominant codec for 4K streaming and is a hard requirement for any pipeline
claiming production-grade 4K support. Its large effort estimate means it should be
planned as a dedicated multi-month project phase rather than appended to an existing
phase.

Suggested sequencing: begin HEVC work after H.264 (Phase 3) is pixel-exact and AV1
(Phase 4) is complete. Target a "good enough for testing" scaffold by v0.3 and a
production-quality decoder by v1.0.
