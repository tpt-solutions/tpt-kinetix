# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
