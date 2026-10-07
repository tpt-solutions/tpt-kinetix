# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.1) - 2026-10-07

### Other

- bump all crates to 0.1.1 and fix crates.io publishing
- Add ffmpeg comparison harness and fix codec bugs it surfaced
- Add per-crate Criterion benches, perf baseline/compare tooling, and publishing docs
- AV1 decoder and coefficient/inter/transform updates; codec crate conformance reporting updates
- Add conformance reporting, AV1 decoder updates, and per-crate changelogs
- remove reference C/oracle files; vision: overhaul reconstruct + deblock/prediction/headers; h264: interlaced + cabac + ref_pic updates; av1: reconstruct/partition; screen/lean/lossless/cli updates
- implement MBAFF B-slice decode (ref lists, field MC gate) + B-frame reconstruction; fix CABAC CBP neighbor context for MBAFF frame pairs
- Fix decoder bitstream-desync and dequant bugs in AV1, H.264, and AAC
- Advance AAC, AV1, and H.264 decode paths, plus realtime and face codec scaffolding
- Advance AAC decode modules, AV1 reconstruction, and H.264 high-profile paths
- Advance H.264/AV1 decode paths and lossless codec wiring
- Add new codec crates (face, lossless, realtime, screen, volumetric) and bitstream foundation

### Fixed

- The frame header now stores per-plane `(width, height)`. The decoder
  previously decoded every plane with the frame-level geometry (plane 0's
  dims), so any frame whose planes are not all the same size — every 4:2:0
  YUV frame, i.e. the primary use case — over-read the smaller chroma
  residual streams and failed with "rANS stream exhausted during
  renormalization". The bitstream format changes (16 bits per plane added to
  the frame header); existing v1 payloads with uneven plane sizes could never
  have decoded correctly.

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
