# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
