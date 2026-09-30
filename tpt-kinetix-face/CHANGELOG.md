# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- Initial `tpt-kinetix-face` crate: an original talking-head / video-conferencing
  codec using landmark-driven parametric synthesis instead of pixel coding.
- `basis` module — 3DMM basis asset + loader (DECISION 2/3/6, step 1) with a
  versioned basis and `basis_hash`, so a decoder with a mismatched basis rejects
  rather than rendering a wrong face.
- `synthesizer` module — deterministic 3DMM rasterizer (DECISION 2, step 4):
  identity + expression basis displacement, pose placement, Lambert/SH shading
  and z-buffer rasterization to RGB24. No neural network on the v1 decode path.
- `params` module — rANS-coded parameter vector (DECISION 3) over the five
  coefficient groups as independent sub-streams via `RansStreamSet`, with a
  zero-biased `FaceCoefModel`.
- `FaceDecoder` end-to-end: sequence header parse, basis load + verify,
  rANS parameter decode, synthesis. Strict mode returns `NotPixelExact` on a
  missing or mismatched basis (DECISION 8).
- `FaceEncoder` assembling a sequence header + key-frame payload from
  `FaceParams` for encode/decode round-trip testing.
- `capabilities()` reports `pixel_exact = false` (synthesized, by design).
- Full design (all 8 resolved decisions) in `docs/face-codec-design.md`.

### Known limitations

- The built-in basis is a deterministic placeholder (procedural head proxy);
  selecting a production 3DMM (FLAME / FaceWarehouse) is open question 1.
