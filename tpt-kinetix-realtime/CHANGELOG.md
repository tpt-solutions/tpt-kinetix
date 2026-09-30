# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- Initial `tpt-kinetix-realtime` crate: an original low-latency, loss-resilient
  video codec for cloud gaming, video conferencing and AR/smart-glasses overlay.
- Profile-agnostic core with three preset parameter sets over one shared format.
- Sequence/frame headers covering the profile-aware parameter sets.
- Fuzz target for the header path.
- Full design (loss recovery, GOP structure, latency budget, validation metric,
  v1 budget, shared-primitive relationship to `tpt-kinetix-lean`) documented in
  `docs/realtime-codec-design.md`.
- Validation harness support via the `realtime-bench` feature
  (`tpt-kinetix-test-utils::realtime_bench`).

### Known limitations

- Block reconstruction, slice framing, intra-refresh masking, hybrid FEC and
  concealment are not implemented; `capabilities()` reports `pixel_exact: false`.
