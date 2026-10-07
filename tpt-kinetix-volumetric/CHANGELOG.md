# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.1) - 2026-10-07

### Other

- bump all crates to 0.1.1 and fix crates.io publishing
- Add per-crate Criterion benches, perf baseline/compare tooling, and publishing docs
- AV1 decoder and coefficient/inter/transform updates; codec crate conformance reporting updates
- Add conformance reporting, AV1 decoder updates, and per-crate changelogs
- add PATENTS.md and gate H.264/AAC behind default-on Cargo features
- AAC PNS noise-scalefactor fix, HLS segment roll-over, and volumetric TMC13 geometry cross-check
- remove reference C/oracle files; vision: overhaul reconstruct + deblock/prediction/headers; h264: interlaced + cabac + ref_pic updates; av1: reconstruct/partition; screen/lean/lossless/cli updates
- implement MBAFF B-slice decode (ref lists, field MC gate) + B-frame reconstruction; fix CABAC CBP neighbor context for MBAFF frame pairs
- Fix decoder bitstream-desync and dequant bugs in AV1, H.264, and AAC
- Merge branch 'master' of https://github.com/tpt-solutions/tpt-kinetix
- Advance AAC, AV1, and H.264 decode paths, plus realtime and face codec scaffolding
- Advance AAC decode modules, AV1 reconstruction, and H.264 high-profile paths
- Advance AAC decode modules, AV1 inter prediction, and face codec scaffolding
- Advance H.264/AV1 decode paths and AAC codebook integration
- Advance H.264/AV1 decode paths and add volumetric codec scaffolding
- Add new codec crates (face, lossless, realtime, screen, volumetric) and bitstream foundation

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- Initial `tpt-kinetix-volumetric` crate: a point-cloud / volumetric codec for
  AR-VR content, for the dominant representation of captured volumetric
  (Depthkit, 8i, LiDAR / depth fusion) content.
- Kinetix framing with magic `b"VOLU"` wrapping G-PCC-faithful coding tools;
  MPEG-I G-PCC TMC13 is the bit-exact conformance oracle (driven through
  `tpt-kinetix-test-utils::tmc13`).
- `tpt-kinetix_core::frame::PointCloud` output type (positions plus per-point
  attribute channels), parallel to `VideoFrame` for the 2D codecs.
- v1 targets a static single cloud: context-modeled occupancy octree geometry
  and region-adaptive predictive (lift, default) or RAHT attributes, both
  lossless and lossy.
- `VolumetricDecoder::capabilities()` reporting `pixel_exact: false`; strict
  mode returns `NotPixelExact`, and any stream declaring the reserved `dynamic`
  flag rejects with `KinetixError::Unsupported`.
- Fuzz target for the header path.
- Full design (all 8 resolved decisions) in `docs/volumetric-codec-design.md`.

### Known limitations

- Geometry (octree) and attribute (lift/RAHT) decode are not yet wired;
  `decode()` returns `Ok(None)` in non-strict mode.
