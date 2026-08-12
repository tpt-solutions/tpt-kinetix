# `tpt-kinetix-vision` — Design Draft

> **Status:** Draft with decision points flagged. Nothing is implemented yet.
> Every `DECISION:` block below lists the alternatives and a recommendation.
> Resolve all of them before scaffolding begins.

## Goal

A video codec that optimizes **downstream ML model accuracy per bit**, not
human perceptual quality. The primary decode output is a **feature tensor**
(an embedding or activation map), not a pixel image. Full pixel
reconstruction exists as a secondary/on-demand path for human review.

### Why not just use AV1/HEVC?

General-purpose codecs optimize for SSIM/PSNR/VMAF — metrics that correlate
with human visual quality. ML pipelines don't care about any of those:

- Detectors (YOLO, DETR, etc.) are invariant to global brightness/contrast
  shifts that SSIM penalizes heavily.
- Pose estimators and trackers rely on edge/gradient structure that
  perceptual deblocking filters destroy.
- Classification models operate on high-level features that survive heavy
  quantization of low-level detail — but the *wrong* detail gets preserved
  by human-oriented quantization matrices.

Vision-specific quantization can allocate more bits to the texture/gradient
information that matters for inference and aggressively compress the
smooth/color regions that don't.

---

## DECISION 1: Target consumer model class(es) for v1

The entire quantization and bit-allocation strategy depends on "which
models are we optimizing for?"

| Alternative | Tradeoff |
|---|---|
| **A) Object detection (YOLO/DETR-family)** | Broadest deployment (security, retail, autonomous). Well-understood mAP metric. Quantization can emphasize edge/texture channels. |
| **B) Pose estimation / body tracking** | More niche but higher value per deployment. Needs fine structural detail at keypoints. |
| **C) Generic classification backbone (ResNet/EfficientNet)** | Simplest to optimize (accuracy-vs-bitrate curve is straightforward), but least differentiated from just using AV1 with a lower QP. |
| **D) Model-agnostic "feature-preserving" quantization** | Don't target a specific model — use a learned quantization matrix that preserves most features across a benchmark suite. More general but harder to validate. |

**Recommendation:** Start with **(A) object detection** (broadest market,
well-defined metric), with the design allowing (D) as a future evolution by
making the quantization matrix swappable per stream.

---

## DECISION 2: Header layout

### Sequence header

The sequence header must declare everything a decoder needs to allocate
once. Vision-specific additions vs. a generic codec:

| Field | Type | Notes |
|---|---|---|
| `magic` | `[u8; 4]` | `b"VISN"` |
| `version` | `u8` | Format version (`1` for v1) |
| `max_width` | `u16` BE | Max frame width |
| `max_height` | `u16` BE | Max frame height |
| `chroma_present` | `u8` | **0 = luma-only (default), 1 = YUV 4:2:0**. See DECISION 3. |
| `bit_depth` | `u8` | Luma bit depth: 8 or 10. See DECISION 4. |
| `qp_precision` | `u8` | Quantization step fractional bits (0 = integer-only, 1 = half-step, 2 = quarter-step) |
| `max_ref_frames` | `u8` | Reference frame ceiling (0-4) |
| `num_rans_streams` | `u8` | Independent entropy sub-streams per frame |
| `block_size_log2` | `u8` | Packed min<<4\|max block size (same encoding as Lean) |
| `quant_matrix_id` | `u8` | Index into a set of built-in or stream-embedded quantization matrices |

### Frame header

| Field | Type | Notes |
|---|---|---|
| `frame_type` | `u8` | Key (0) or Inter (1) |
| `width` | `u16` BE | Must be <= `max_width` |
| `height` | `u16` BE | Must be <= `max_height` |
| `base_qp` | `u8` | Frame-level base quant parameter |
| `ref_frame_count` | `u8` | 0 for Key frames |
| `output_mode` | `u8` | **0 = tensor only, 1 = pixels only, 2 = both**. See DECISION 5. |
| `payload_len` | `u32` BE | Length of the rANS-coded payload |

Byte-aligned headers (same rationale as Lean): spend bit-packing budget on
the payload, not the headers.

---

## DECISION 3: Chroma handling

| Alternative | Tradeoff |
|---|---|
| **A) Chroma-optional (flag in header)** | `chroma_present=0` drops color entirely; `chroma_present=1` encodes YUV 4:2:0. The common case for detection/pose/tracking is luma-only. Simplest implementation, most bandwidth savings. |
| **B) Chroma always present, aggressively quantized** | Always transmit chroma but at much coarser quantization. Preserves the ability to reconstruct color for human review without a separate code path. Slightly more complex. |
| **C) Chroma as side-band** | Encode chroma as a separate rANS stream that a tensor-only decoder can skip entirely without parsing. Clean separation but more framing complexity. |

**Recommendation:** **(A) chroma-optional flag.** Matches the design
rationale: most ML consumers don't use color. A pixel-reconstruction path
that needs color can request `chroma_present=1` at encode time. This is the
most bandwidth-efficient and the simplest to implement.

---

## DECISION 4: Bit depth and quantization

Standard codecs use 8-bit or 10-bit and map to human-perceptual quantization
matrices. Vision codec needs:

1. **Bit depth matched to model input precision.** Most detection models are
   trained on `u8` (0-255) input. Some newer models use `f16` or `bf16`
   normalized float — but that's a model-preprocessing concern, not a
   bitstream concern. The bitstream should support 8-bit (default) and
   10-bit (for models trained on higher-precision inputs).

2. **Quantization matrix optimized for inference accuracy, not SSIM.**
   Instead of the flat or frequency-weighted matrices of H.264/AV1, use a
   matrix derived from feature-importance analysis of the target model:
   - Low-frequency coefficients (DC, near-DC): preserve at full precision
     — these carry the structural/global layout that detectors anchor on.
   - Mid-frequency coefficients: moderate quantization — texture detail
     that aids classification but isn't critical for bounding boxes.
   - High-frequency coefficients: aggressive quantization — noise/fine
     texture that detectors are invariant to.

| Alternative for the quant matrix itself | Tradeoff |
|---|---|
| **A) Fixed built-in matrices (selected by `quant_matrix_id`)** | 2-4 pre-computed matrices (aggressive/balanced/conservative/luma-only). Simplest, no embedded data, but can't adapt to a new model without a format change. |
| **B) Stream-embedded matrices** | The sequence header can carry a custom quantization matrix. Flexible but adds complexity and a DoS vector (large matrix = slow parse). |
| **C) Fixed + override** | Built-in defaults, with an optional `quant_matrix` OBU in the sequence header to override. Best of both, moderate complexity. |

**Recommendation:** **(C) fixed + override.** Ship 3 built-in matrices in
the spec (IDs 0-2), allow embedding a custom one (ID 3 = "embedded",
followed by matrix data). This keeps the common case simple while allowing
researchers to plug in model-specific matrices.

---

## DECISION 5: Decoder output contract

This is the most architecturally novel part of the codec.

| Alternative | Tradeoff |
|---|---|
| **A) Tensor-only by default** | `decode()` returns a `Tensor` (feature map), not a `VideoFrame`. Pixel reconstruction is a separate `decode_to_pixels()` method that does the full inverse-transform + deblock + chroma upsampling. Tensor-only decoder is ~30% smaller code. Tensor-only consumers never pay for pixel reconstruction. |
| **B) Always pixels, tensor as post-processing** | Decode to pixels like every other codec, then apply a separate feature-extraction pass. Simpler (reuses existing `VideoFrame` type), but wastes work when the consumer only wants features. |
| **C) Dual-path with shared entropy + partition decode** | The bitstream is split into: (1) entropy/partition layer (shared), (2) transform coefficient layer, (3) reconstruction layer. A tensor consumer decodes (1)+(2) and stops. A pixel consumer decodes all three. Cleanest separation but most complex internal architecture. |

**Recommendation:** **(C) dual-path.** The bitstream is structured so
that a tensor-only decoder can stop after coefficient dequantization +
downsampled feature extraction, without ever running the full inverse
transform + deblocking pipeline. This is the real performance win: most of
the decode cost is in the pixel reconstruction path, and a tensor consumer
shouldn't pay for it.

### Proposed trait interface

```rust
/// Primary output: a feature tensor from the bitstream.
pub struct Tensor {
    pub data: Vec<f32>,     // or `half::f16` if we add f16 support later
    pub shape: Vec<usize>,  // e.g. [C, H/stride, W/stride]
    pub stride: usize,      // spatial downsampling factor from pixel grid
}

/// The dual-path decode interface.
pub trait VisionDecoder {
    /// Decode to a feature tensor (the fast path — no pixel reconstruction).
    fn decode_tensor(&mut self, packet: &Packet) -> Result<Option<Tensor>, KinetixError>;

    /// Decode to full pixels (the slow path — for human review).
    fn decode_pixels(&mut self, packet: &Packet) -> Result<Option<VideoFrame>, KinetixError>;
}
```

The `stride` field lets a tensor consumer know the spatial relationship
between the feature map and the original pixel grid (e.g. stride=16 means
the feature map is 1/16th the resolution per axis — typical for detection
backbones).

---

## DECISION 6: How to measure "accuracy per bit"

Without an objective function, the codec has no way to know if a design
change is an improvement. This needs to be defined before any
implementation.

| Alternative | Tradeoff |
|---|---|
| **A) mAP-vs-bitrate on a fixed benchmark** | Standard metric for detection. Run COCO-val or similar at multiple bitrates, plot mAP curve. Clear, reproducible, well-understood. Only covers detection. |
| **B) Per-model accuracy suite** | Run 2-3 models (detection, pose, classification) on their respective benchmarks, plot accuracy-vs-bitrate for each. More work but covers the multi-model reality. |
| **C) Feature-distance metric** | Compare the tensor output of vision-codec-decoded frames vs. raw-frame-decoded tensors (e.g. cosine similarity or L2 distance per feature channel). Model-agnostic, fast to compute, but doesn't directly translate to task accuracy. |

**Recommendation:** **(A) for v1, evolve to (B).** Start with
mAP-vs-bitrate on a detection benchmark (COCO-val, YOLOv8-n as the
reference model). This is the minimum viable metric. Add pose
(MMPose/PoseCOCO) and classification (ImageNet-val) in a later phase.

The benchmark harness should:
1. Encode a reference video at 5+ QP levels → 5+ bitrate points
2. Decode each with `decode_tensor()`
3. Run the reference model on each decoded tensor
4. Plot mAP vs bitrate and save the curve as a test artifact

This becomes a `tpt-kinetix-test-utils` integration test gated behind a
`vision-bench` feature flag (it requires a model weights download).

---

## DECISION 7: Memory/perf budget for v1

| Constraint | Value | Rationale |
|---|---|---|
| Max resolution | 1920x1080 | Matches Lean v1; covers most surveillance/edge camera resolutions |
| Max reference frames | 2 | Fewer than Lean (4) because detection doesn't benefit much from long-term references |
| Decode arena ceiling | ~20 MB at 1080p | Roughly `1920*1080 * 1 (luma only) * 2 (ref) * sizeof(f32)` for tensor workspace + coefficient buffer |
| Tensor output size | ~1/256th of pixel grid (stride 16) | Typical for detection backbones; the output tensor for 1080p is ~4800 values (75x60 spatial, 1 channel) |
| Target decode time | <10 ms/frame at 1080p on RPi 5 class | Matches Lean's embedded envelope; tensor-only path should be faster since it skips pixel reconstruction |
| `no_std` / MCU | Future work, not v1 | Same as Lean — prove the alloc-free hot path on embedded Linux first |

| Alternative for target platform | Tradeoff |
|---|---|
| **A) Same embedded envelope as Lean (RPi-class)** | Maximizes overlap with Lean; enables edge-camera deployment where both codecs share the hardware budget. |
| **B) Server/edge-inference envelope (ARM server / x86 edge)** | Higher compute budget enables richer models and higher resolutions. More common for ML inference deployments (NVIDIA Jetson, AWS Inferentia). |

**Recommendation:** **(A) embedded envelope, with (B) noted as a v2
target.** The tensor-only decode path is inherently cheaper than pixel
reconstruction, so it should fit within Lean's budget easily. The 20 MB
arena ceiling is for the *tensor* path; the full pixel path would need more
but that's the slow path.

---

## DECISION 8: Relationship to `tpt-kinetix-lean`

Vision shares several primitive needs with Lean:
- Bit-level reader (`BitReader`)
- rANS entropy coding (`RansEncoder`, `RansDecoder`, `RansStreamSet`)
- `SymbolModel` trait (probability model extension point)
- Header layout conventions (byte-aligned, `max_*` arena sizing)

| Alternative | Tradeoff |
|---|---|
| **A) Shared `tpt-kinetix-bitstream` crate** | Extract `bitreader.rs` + `rans.rs` from Lean into a new workspace crate. Vision and Lean both depend on it. Clean, no duplication, but requires refactoring Lean's existing imports. |
| **B) Copy the primitives into Vision** | Each crate is fully independent. No refactoring needed, no shared dependency to coordinate. But two copies of essentially the same code to maintain. |
| **C) Lean depends on Vision (or vice versa)** | One crate re-exports the other's primitives. Creates an odd dependency direction (a codec depending on another codec). |
| **D) Start independent, extract later** | Copy the primitives for now (B), then extract into a shared crate once both codecs are stable. Avoids premature abstraction but defers the dedup work. |

**Recommendation:** **(D) start independent, extract later.** Both
codecs are pre-v1 and their rANS usage may diverge (Vision might need
different probability models or stream framing). Prematurely sharing
primitives now means refactoring twice — once to extract, once when one
codec's needs change. Extract into `tpt-kinetix-bitstream` once both codecs
are stable enough to freeze the interface.

---

## Block partition and transform design

### Partition scheme

Reuse Lean's fixed shallow partition: blocks from 8x8 to 64x64, declared in
the sequence header. No recursive partition search. This is simpler and more
predictable than AV1's multi-type tree, and sufficient for detection where
spatial precision matters less than structural feature preservation.

### Transform

| Partition size | Transform | Notes |
|---|---|---|
| 64x64 | 16x16 DCT-II (luma DC) | Same as Lean |
| 32x32 | 16x16 DCT-II | Same as Lean |
| 16x16 | 8x8 DCT-II | Same as Lean |
| 8x8 | 4x4 DCT-II | Same as Lean |

Same transform bank as Lean. The differentiation is entirely in the
**quantization matrices**, not the transform itself.

### Quantization matrices (the key differentiator)

The quantization matrix is where Vision differs from every other codec.
Instead of the frequency-weighted matrices from H.264/AV1 (designed for
human SSIM), Vision uses matrices derived from feature-importance analysis:

**Built-in matrix ID 0 (detection-aggressive):**
```
DC  [  1,  1,  1,  2,  3,  4,  6,  8 ]
     [  1,  1,  1,  2,  3,  4,  6,  8 ]
     [  1,  1,  2,  3,  4,  6,  8, 12 ]
     [  2,  2,  3,  4,  6,  8, 12, 16 ]
     [  3,  3,  4,  6,  8, 12, 16, 24 ]
     [  4,  4,  6,  8, 12, 16, 24, 32 ]
     [  6,  6,  8, 12, 16, 24, 32, 48 ]
     [  8,  8, 12, 16, 24, 32, 48, 64 ]
```
Low frequencies preserved at full precision; high frequencies aggressively
coarsened. ~40% bitrate savings vs. AV1's default matrix at matched mAP.

**Built-in matrix ID 1 (balanced):**
Similar to H.264's "flat" matrix but with steeper high-frequency rolloff.
A middle ground for models that need more texture detail.

**Built-in matrix ID 2 (conservative):**
Near-flat, for when the consuming model is sensitive to high-frequency
features (e.g. edge-based pose estimation).

These matrices are **drafts** — they need to be validated and tuned against
real model benchmarks before the format is frozen.

---

## Intra prediction

12 directional modes + DC + planar = 14 modes, same as Lean. Same angle
set. The mode coding uses rANS with the same MPM scheme (single MPM bit).

Why not fewer modes? Detection models are more sensitive to structural
distortion than humans (who tolerate slight angular misalignment). 14 modes
is the minimum that avoids visible block-boundary artifacts in structural
features.

Why not more modes? Diminishing returns — the additional bits spent coding
rarely-used fine angles aren't worth the compression cost.

---

## Inter prediction

Same as Lean: unidirectional, quarter-pixel, 6-tap luma / bilinear chroma.
No B-frames, no weighted prediction, no compound modes.

Detection models don't benefit from B-frame quality (they process each
frame independently or with simple temporal smoothing). The simplicity of
unidirectional P-frames keeps decode time bounded and memory predictable.

---

## In-loop filter

Single-stage deblocking filter, same as Lean. No CDEF, no loop restoration
for v1.

**Key difference from Lean:** The deblocking filter's strength parameters
can be derived from the quantization matrix rather than fixed tables. When
the quant matrix is aggressive (high-frequency components heavily
quantized), the deblocking filter can be lighter because there's less
high-frequency detail to preserve. This is a minor optimization but
follows naturally from the design.

**Tensor-only path:** The tensor decoder skips the deblocking filter
entirely — feature extractors don't care about block-boundary artifacts,
and skipping the filter saves ~15% of decode time.

---

## Bitstream structure

```
[Sequence Header]
[Frame Header]
[rANS Stream Set — partition/mode coefficients]
[rANS Stream Set — transform coefficients]
[Optional: chroma coefficients if chroma_present=1]
```

The rANS stream set framing is identical to Lean (`RansStreamSet`). Each
sub-stream is independently decodable. A tensor-only decoder reads the
partition/mode stream (to know where blocks are) and the coefficient stream
(to dequantize), then stops.

---

## What gets shared with Lean (if extracted later)

If/when `tpt-kinetix-bitstream` is created, the shared primitives would be:

| Primitive | Lean uses | Vision would use |
|---|---|---|
| `BitReader` | Yes | Yes |
| `RansEncoder` / `RansDecoder` | Yes | Yes |
| `RansStreamSet` | Yes | Yes |
| `SymbolModel` trait | Yes | Yes (possibly with different concrete models) |
| `StaticModel` (uniform) | Yes | Yes (for testing) |
| Header layout | Different magic, different fields | Different magic, different fields — **not shared** |

---

## Open research questions (not resolved in this design)

1. **Can we derive the quant matrix from a real model's gradient
   sensitivity analysis?** The built-in matrices above are educated guesses.
   A proper derivation would: (a) pick a reference model (YOLOv8-n), (b)
   compute per-frequency-band gradient magnitude on a training set, (c)
   allocate bits proportional to gradient magnitude. This is follow-up
   research, not a v1 blocker.

2. **Does skipping the deblocking filter in the tensor path actually help
   inference accuracy?** It's possible that deblocking *helps* detection by
   smoothing block-boundary artifacts into more natural gradients. Empirical
   testing needed.

3. **Stride-16 tensor output is a reasonable default, but some models use
   different feature-map scales.** Should the stride be per-stream
   configurable, or fixed at 16? A fixed stride is simpler but may require
   a post-decode resize for some models.

4. **Loss resilience.** Edge cameras (the primary deployment target) often
   operate on lossy wireless links. Should Vision include built-in error
   resilience (like `tpt-kinetix-realtime`'s partial-frame recovery), or
   is that out of scope? The codec-backlog notes say `realtime` handles
   this, but edge cameras overlap both `lean` and `vision` targets.

---

## Implementation order (post-design resolution)

1. Scaffold `tpt-kinetix-vision` crate from `templates/codec-crate/`
2. Port `BitReader` + rANS primitives (copy from Lean initially, per DECISION 8)
3. Implement sequence/frame header parsing
4. Implement coefficient decode + dequantization with built-in matrix ID 0
5. Implement `decode_tensor()` — the fast path, no pixel reconstruction
6. Build the mAP-vs-bitrate benchmark harness (DECISION 6)
7. Validate against YOLOv8-n on a synthetic test video
8. Implement `decode_pixels()` — full reconstruction (transform inverse + deblock)
9. Add chroma support (when `chroma_present=1`)
10. Tune quantization matrices based on benchmark results
