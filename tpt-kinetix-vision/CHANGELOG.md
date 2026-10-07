# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.1) - 2026-10-07

### Other

- bump all crates to 0.1.1 and fix crates.io publishing
- AV1 SIMD helpers and bitreader speedups; H.264 env-debug hooks; VP9 div128 fixture notes
- cache Hadamard matrices, remove per-block allocations
- Add per-crate Criterion benches, perf baseline/compare tooling, and publishing docs
- AV1 decoder and coefficient/inter/transform updates; codec crate conformance reporting updates
- Add conformance reporting, AV1 decoder updates, and per-crate changelogs
- read_lr gating now matches dav1d (unit-alignment + frame-boundary) — the 106 extra entropy reads per tile row are gone; the EC desync at the non_uniform_tiling (112,56) skip read should be resolved
- remove reference C/oracle files; vision: overhaul reconstruct + deblock/prediction/headers; h264: interlaced + cabac + ref_pic updates; av1: reconstruct/partition; screen/lean/lossless/cli updates
- implement MBAFF B-slice decode (ref lists, field MC gate) + B-frame reconstruction; fix CABAC CBP neighbor context for MBAFF frame pairs
- implement full block reconstruction (intra+inter, WHT, deblock) with DPB; screen: implement mode classifier + flat/glyph/NATURAL reconstruction; vision: add headers/prediction/quant/transform/deblock/reconstruct modules; av1: remove partition debug instrumentation; update todos
- *(deps)* bump nom from 7.1.3 to 8.0.0
- Fix H.264 inter CBP table, coeff_token FLC codes, and dec_ref_pic_marking gating
- Wire H.264 CAVLC I-slice decode into decoder.rs, scaffold tpt-kinetix-vision crate

### Changed

- **Performance: the Walsh–Hadamard transform matrices are cached instead of
  rebuilt on every call.** `hadamard_2d_raw` rebuilt the matrix on *every*
  invocation — a nested `Vec<Vec<i32>>` (`1 + n` heap allocations) plus
  `O(n² log n)` work — and it runs once per block per frame in both the encode
  and the decode direction. The matrices are pure functions of `n` and `n` only
  ever takes a few power-of-two values, so they are now built once per size
  behind a `OnceLock` (thread-safe for concurrent decode). Entries and
  accumulation order are unchanged, so the transform stays bit-exact.
- **Performance: the per-block hot loops no longer allocate.** Reconstruction
  allocated ~5 `Vec`s per block and the encoder ~6 more per intra mode trial
  (14 modes per block, so ~84 allocations per block). Both directions now share
  one per-frame scratch (`BlockScratch` / `EncodeScratch`).
- `predict_directional`'s extended top/left arrays moved from two per-block
  `vec!` allocations to fixed stack scratch (bounded and asserted at run time).
- New `transform::inverse_2d_with_scratch` for callers that already own a
  buffer; `inverse_2d` is now a thin allocating wrapper over it.
- New `headers::MAX_BLOCK_SIZE` / `MAX_BLOCK_SIZE_LOG2` constants, so the
  scratch bound is named rather than a magic 64. The parser already rejected
  anything outside the 8x8..64x64 range, so this is a named constant rather
  than new validation.

Measured with `cargo bench -p tpt-kinetix-vision` at 1920x1080:

- `encode`: **+19.0%**
- `decode_pixels`: **+18.7%**
- `decode_tensor`: unchanged (reported -5.8% in the same run, but that path is
  a pure block parser — it calls neither the transform nor the scratch, so the
  figure is machine noise rather than a regression)

Output is unchanged: all 33 vision tests pass, including the pixel and tensor
round-trip suites.

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- Initial `tpt-kinetix-vision` crate: an original video-for-machines codec
  optimizing downstream ML model accuracy per bit instead of human perceptual
  quality (no external reference oracle exists).
- `decode_tensor()` fast path — entropy + dequantization + stride-16 pooling
  into a feature `Tensor`, with no inverse transform, prediction or deblocking.
- `decode_pixels()` slow path — full reconstruction into a `VideoFrame`:
  14-mode intra + unidirectional-P inter prediction, Walsh-Hadamard transform
  and deblocking.
- 8-bit decode with `chroma_present = 0` (luma-only) or 4:2:0, block sizes
  8x8..64x64, and three built-in ML-weighted quantization matrices.
- Declared-but-unimplemented features reject with `KinetixError::Unsupported`
  (10-bit, fractional qp, multi-stream entropy, embedded quant matrices).
- `capabilities()` reports `pixel_exact: false` by design; strict mode returns
  `NotPixelExact`.
- Full design specification in `docs/vision-codec-design.md`.
