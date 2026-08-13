# `tpt-kinetix-lossless` — Design Draft

> **Status:** Draft with decision points. Items 1, 2, 6 are **resolved**
> (decided 2026-08-13). Items 3, 4, 5 carry a **recommendation** in this doc
> and should be confirmed before scaffolding begins.
>
> This doc is the home for the `tpt-kinetix-lossless` design-phase checklist in
> `todo.md` (Phase 14). Each checklist item maps to a `DECISION:` block below.
> Nothing is implemented yet.

## Goal

An **original, bit-exact reversible** codec for high-bit-depth still/frame data
— not a perceptual codec. It targets medical imaging (DICOM CT/MR/X-ray),
scientific capture (sensor / frame-grabber feeds), and archival preservation of
high-bit-depth masters. The defining property is **guaranteed lossless
round-trip**: decode of an encoded frame reproduces the original samples exactly
(per the `DECISION 3` checksum contract, not just external testing).

Unlike `tpt-kinetix-lean` (bounded-time embedded *lossy* video) or AV1/HEVC
(lossy, perceptual), this codec has no quality axis: every preserved bit is
verified. The design optimizes for **reversibility + high bit depth first**, and
compression ratio second — explicitly trading some ratio for a format that is
auditable and provably lossless.

### Why not just use FFV1 / lossless HEVC / lossless AV1?

- **FFV1** is the closest fit (bit-exact, integer, well-proven) but is a single
  self-contained design; this crate exists to (a) reuse Kinetix's own entropy
  primitives and (b) reserve a wavelet mode for a later phase without a format
  break.
- **Lossless HEVC / AV1** are add-on modes of perceptual codecs: larger, more
  complex decode, and not "simple/embeddable" the way a dedicated integer
  predictor + rANS path is.

The codec-backlog entry (`docs/codec-backlog.md`) frames the gap exactly:
*"Perceptually close enough" is disqualifying for this use case; existing
lossless modes aren't simple/embeddable.*

---

## DECISION 1: Target domain for v1 + bit-depth range

*Resolved (2026-08-13).*

| Alternative | Tradeoff |
|---|---|
| A) Medical imaging only | Cleanest conformance target (DICOM 10/12/16-bit grayscale, FFV1 baseline), but excludes the scientific/archival data that motivated the crate. |
| B) Scientific capture only | Heterogeneous sensors, often 12/16-bit, less standardized container — more variable scope. |
| C) Archival only | Widest bit-depth range, no live-capture timing — but broad and unfocused for a v1. |
| **D) Unified format for all three** | One bitstream serves medical + scientific + archival. Slightly more header surface (per-plane bit depth, color/sample model), but avoids three niche formats. **Chosen.** |

**Decision:** a **single unified format** serving all three domains, guaranteeing
bit-exact round-trip for **10/12/16-bit** samples. The bit depth is a per-plane
field in the sequence header (not a fixed format constant), so the same decoder
handles a 10-bit medical grayscale plane and a 16-bit scientific RGB master.
8-bit and >16-bit are explicitly out of scope for v1 (the `DECISION 5` arena and
checksum widths are sized for ≤16-bit).

---

## DECISION 2: Reversible compression approach

*Resolved (2026-08-13):* **predictive + entropy (FFV1-like) as the v1 primary,
with a reversible wavelet mode reserved in the bitstream for a later phase.**

| Alternative | Tradeoff |
|---|---|
| A) Predictive + entropy (FFV1-like) | Per-sample intra prediction (median/context-adaptive) + rANS entropy coding. Simplest path to bit-exact; easiest to checksum; clear conformance target (FFV1). Lower ratio than wavelets on very smooth data, but robust and auditable. |
| B) Reversible integer wavelet (JPEG2000-style CRF 5/3 / 9/7) | Higher ratio on smooth medical/scientific data, but more complex, harder edge cases (boundary handling, lifting reversibility), larger decode budget. |
| **C) Predictive primary + wavelet mode reserved** | Ship A as v1; reserve a `transform_id` field so B can be signaled later without a format change. More design surface now, but no future break. **Chosen.** |

### v1 reversible pipeline (mode = predictive)

1. **Plane model.** Each plane is coded independently. Pixel samples are treated
   as unsigned integers in `[0, 2^bit_depth − 1]`.
2. **Intra prediction (reversible).** Per-sample context predictor:
   - Default **median** of (left, up, up-left) neighbours, exactly FFV1's
     `PR = MED(L, U, UL)` for the 0-context case.
   - Optional **context-adaptive** predictor for 12/16-bit: a small set of
     prediction contexts (selected by local gradient / neighbour variance),
     signalled per-slice. All integer; no quantization → fully reversible.
3. **Residual.** Store `sample − predictor` (wrapped to the unsigned sample
   range) so the decode `predictor + residual` reconstructs the exact sample.
4. **Entropy coding.** Residuals coded with **rANS**, reusing
   `tpt-kinetix-lean`'s `Rans` primitive (`DECISION 6`), with per-context
   frequency tables (context = prediction context + quantized neighbour
   residual magnitude, à la FFV1's `quant_table`).
5. **Reserved mode.** The slice/plane header carries `transform_id`: `0 =
   predictive` (v1). A future `1 = reversible integer wavelet` reuses the same
   entropy framing; the wavelet lifting stages are added behind the same
   `DECISION 3` checksum contract. The bitstream is forward-compatible: a v1
   decoder that sees `transform_id != 0` returns a typed
   `UnsupportedTransform` error rather than decoding to silent garbage.

### Reversibility invariant

Every stage (prediction, residual formation, entropy) is bidirectional and
integer-only. There is **no quantization step anywhere in the codec** — this is
what makes the `DECISION 3` checksum a tautology-to-verify rather than a
statistical hope.

---

## DECISION 3: Decode-path correctness contract (built-in checksum)

*Recommendation (pending confirmation).* Make bit-exactness part of the format,
not just external testing:

- **Per-frame checksum.** The frame header embeds a checksum of the
  reconstructed samples: `CRC32` for 10/12-bit planes and `CRC64` for 16-bit
  (64-bit width because 16-bit × large planes exceed CRC32's collision comfort).
  The decoder computes the checksum over its reconstructed plane and **must**
  match it; a mismatch returns a typed `ReversibilityError` (not `Ok` with
  wrong data — consistent with the Kinetix honesty contract every decoder
  follows, see `tpt-kinetix-h264`/`tpt-kinetix-av1`).
- **Per-stream checksum.** The sequence/file header embeds a `SHA-256` over the
  entire encoded stream, so corruption anywhere (including header fields) is
  caught before decode.
- **Round-trip test harness.** `tpt-kinetix-test-utils` gains a
  `lossless_roundtrip` harness asserting `decode(encode(x)) == x` for
  `x` drawn from the 10/12/16-bit corpus, gated on the built-in checksums.

This turns "is it actually lossless?" into something the decoder *enforces* on
every frame, satisfying the checklist item "verified as part of the format."

---

## DECISION 4: Compression-ratio measurement for design validation

*Recommendation (pending confirmation).* Define the validation baseline and
metric before any tuning:

- **Baseline codecs:** FFV1 (v3, the closest integer-lossless peer) and
  lossless HEVC, run with `ffmpeg`. Both produce bit-exact output, so the
  comparison is ratio-only.
- **Corpus:** 10/12/16-bit grayscale + RGB samples spanning the three domains —
  DICOM CT/MR (medical), sensor / frame-grabber captures (scientific), and
  high-bit-depth archival masters. Synthetic ramp/noise/flat patterns included
  as edge cases.
- **Metric:** **ratio at guaranteed losslessness** — bits-per-pixel (bpp) with
  *no* quality axis (lossless has none). Primary report is
  `ratio_vs_FFV1 = size_FFV1 / size_kinetix_lossless` per sample, aggregated as
  mean/median. The v1 acceptance bar: **within ~10% of FFV1** on the corpus
  (the same ~10–15% "worse than the best" tradeoff Lean accepts for its
  constraints), with wavelet mode (future) closing the gap on smooth data.
- **Honesty:** if a sample decodes non-bit-exact (fails `DECISION 3`), it is
  reported as a **conformance failure**, never as a ratio number.

---

## DECISION 5: Memory / perf budget for v1

*Recommendation (pending confirmation).*

- **Max resolution (per plane):** `4096 × 4096` — archival/scientific frames
  are large but bounded; declared in the sequence header as `max_width` /
  `max_height` so the decoder sizes its arena **once at stream start and never
  allocates on the per-frame path** (same bounded-memory principle as Lean).
- **Decode arena ceiling:** `max_width * max_height * planes * (bit_depth/8)`,
  with no per-frame growth. For 16-bit RGBA at 4096² this is ~128 MB — the
  explicit v1 ceiling; larger is a future profile.
- **Parallelism:** single-threaded reversible prediction; entropy decode is
  parallel across independently-decodable rANS **sub-streams** (reusing Lean's
  interleaved-substream framing from `DECISION 6`), so the entropy stage
  parallelizes without a CABAC-style serial bottleneck.
- **Math:** integer-only, no floating point — keeps the decode path auditable
  and FPU-free, matching the project's lean toward verifiable integer pipelines.
- **No real-time requirement.** Unlike `tpt-kinetix-realtime` / `lean`, this
  codec has no latency/deadline budget; "large uncompressed frame sizes" are
  expected and sized for, not avoided.

---

## DECISION 6: Relationship to existing kinetix bitstream primitives

*Resolved (2026-08-13):* **reuse the shared `tpt-kinetix-bitstream` crate for
`BitReader` / `Rans` primitives** (these originated in `tpt-kinetix-lean` and
were extracted into `tpt-kinetix-bitstream` per the realtime codec's DECISION 7,
the "third hand-rolled reader" consolidation the open question below anticipated).

- `tpt-kinetix-lossless` **depends on** `tpt-kinetix-bitstream` for entropy
  primitives (`bitreader::BitReader`, the `rans` framing) rather than re-deriving
  them — consistent with the Phase 13/14 intent to share a bitstream-utility
  layer. The lossless crate adds its own **reversible prediction/residual**
  stage (Lean's transform bank is lossy DCT-based and is *not* reused for the
  prediction — lossless needs integer-reversible prediction instead).
- The `RansStreamSet` framing (interleaved, independently-decodable sub-streams
  per `DECISION 5` parallelism) is shared with Lean/Vision/realtime unchanged,
  now via `tpt-kinetix-bitstream`.
- The earlier open question (factor a shared `tpt-kinetix-bitstream` crate) is
  **now resolved**: that crate exists and is the home of these primitives, so the
  lossless crate consumes them directly rather than via a Lean re-export.

---

## Implementation order (post-design resolution)

1. Confirm DECISIONs 3, 4, 5 in this doc.
2. Scaffold `tpt-kinetix-lossless` from `templates/codec-crate/`, add to
   workspace `[members]` and `release-plz.toml` publish list (note: unlike `aac`/
   `lean`/`vision`, this one *is* intended for publish — confirm).
3. Sequence/frame header parsing (byte-aligned, `max_*` arena sizing like Lean).
4. Reversible predictive path: median + context-adaptive predictor → residual →
   rANS (reusing `tpt-kinetix-bitstream`) → reconstruct. Prove bit-exact on a
   synthetic corpus.
5. Wire `DECISION 3` checksums (per-frame CRC32/CRC64 + stream SHA-256) into
   encode + decode, returning `ReversibilityError` on mismatch.
6. Build the `DECISION 4` ratio harness vs FFV1 / lossless HEVC; tune contexts.
7. Reserve `transform_id` wavelet mode in the header; (later phase) implement
   the reversible integer wavelet behind the same checksum contract.
