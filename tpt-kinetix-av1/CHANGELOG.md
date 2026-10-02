# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Performance: the deblocking loop filter no longer allocates.** The filter
  is the hottest phase of AV1 decode (Phase 2 of `todo-perf.md` measured it at
  62% of a 320x240 frame and 70% at 720p), and it was heap-allocating **two
  `Vec<i32>` per filtered row** — a line buffer and the result — inside the
  innermost loop. Both deblocking passes now filter through a reusable
  16-sample stack buffer, since the filter only ever reads 7 taps before and 6
  after the edge. Measured on the cached `testsrc` corpus (decode only, phase
  timers off, A/B by stashing `loop_filter.rs`):
  - 320x240: 36.23s -> 30.42s for 12000 frames (**-16.0%**)
  - 1280x720: 130.59s -> 110.71s for 3600 frames (**-15.2%**)
- **Internal refactor:** `filter_line_1d` is split into an allocating
  `filter_line_1d` wrapper and an allocation-free `filter_line_1d_into` core
  that writes into a caller-owned buffer. The allocating form is now
  `#[cfg(test)]` and is kept as the reference oracle for the shared-buffer path,
  which is what the existing filter unit tests exercise.

Decoded output is **unchanged**: the FATE corpus is still 204/204 bit-exact vs
libdav1d, `libaom_crosscheck` and `phase_c_conformance` still report zero luma
difference, and all 173 AV1 lib tests pass.

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- *(stream)* RTMP AMF connect/publish + FLV depacketization, MPEG-TS HLS muxing
- DecoderCapabilities introspection + MP4 muxer crate

### Other

- rename kinetix-* crates to tpt-kinetix-*, add probe subcommand and CI jobs
