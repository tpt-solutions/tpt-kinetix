# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- Initial `tpt-kinetix-lossless` crate: a bit-exact reversible codec for
  high-bit-depth still/frame data — medical imaging (DICOM CT/MR/X-ray),
  scientific capture (sensor / frame-grabber feeds) and archival preservation.
- Unified v1 format across medical / scientific / archival domains, guaranteeing
  bit-exact round-trip for 10/12/16-bit samples via a per-plane `bit_depth`
  field.
- Predictive + entropy path (FFV1-like): per-sample prediction from
  left/up/up-left neighbours via the FFV1 median predictor, with the signed
  residual Rice-coded under a per-sample adaptive parameter.
- Built-in reversibility contract (DECISION 3): every plane carries a CRC of
  its reconstructed samples (CRC-32 below 16-bit, CRC-64 for 16-bit); the
  decoder verifies it and errors on mismatch.
- Forward compatibility: a `transform_id` field reserves a reversible-wavelet
  mode; a v1 decoder rejects any non-zero `transform_id` with
  `KinetixError::Unsupported` rather than decoding to silent garbage.
- Reuses the shared `tpt-kinetix-bitstream` primitives (`BitReader`, rANS).
- Full design (DECISION 1-6) in `docs/lossless-codec-design.md`.

### Known limitations

- Entropy is currently a simple adaptive Rice coder; swapping in the shared
  rANS with a context-adaptive model and adding the reserved wavelet mode are
  follow-up work (DECISION 2/4).
